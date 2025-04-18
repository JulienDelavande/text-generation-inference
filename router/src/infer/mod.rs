// pub(crate) mod v2;
mod chat_template;
pub mod tool_grammar;

use crate::validation::{ValidGenerateRequest, Validation, ValidationError};
use crate::Tool;
use crate::{
    ChatTemplateVersions, FinishReason, GenerateRequest, HubProcessorConfig, HubTokenizerConfig,
    Message, PrefillToken, Token,
};
use async_stream::stream;
use async_trait::async_trait;
use axum::response::sse::Event;
use chat_template::ChatTemplate;
use futures::future::try_join_all;
use futures::Stream;
use minijinja::ErrorKind;
use serde::Serialize;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use thiserror::Error;
use tokio::sync::{OwnedSemaphorePermit, Semaphore, TryAcquireError};
use tokio::time::Instant;
use tokio_stream::wrappers::UnboundedReceiverStream;
use tokio_stream::StreamExt;
use tracing::instrument;
use nvml_wrapper::Nvml;

#[async_trait]
pub trait Backend {
    fn schedule(
        &self,
        request: ValidGenerateRequest,
    ) -> Result<UnboundedReceiverStream<Result<InferStreamResponse, InferError>>, InferError>;

    async fn health(&self, current_health: bool) -> bool;

    /// The state of the health on startup
    /// Typically false, or true if the backend includes
    /// a warmup phase.
    fn start_health(&self) -> bool {
        false
    }

    fn name(&self) -> &'static str;
}

/// Inference struct
#[derive(Clone)]
pub struct Infer {
    /// Validation
    validation: Validation,
    /// Request backend
    backend: Arc<dyn Backend + Send + Sync>,
    /// Chat template
    pub(crate) chat_template: Option<ChatTemplate>,
    /// Inference limit
    limit_concurrent_requests: Arc<Semaphore>,
    /// Backend health
    backend_health: Arc<AtomicBool>,
    /// NVML instance
    nvml: Arc<Nvml>,
}

impl Infer {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        backend: impl Backend + Send + Sync + 'static,
        validation: Validation,
        max_concurrent_requests: usize,
        tokenizer_config: HubTokenizerConfig,
        processor_config: HubProcessorConfig,
    ) -> Self {
        let chat_template = tokenizer_config
            .chat_template
            .or(processor_config.chat_template)
            .and_then(|t| match t {
                ChatTemplateVersions::Single(template) => Some(template),
                ChatTemplateVersions::Multiple(templates) => templates
                    .into_iter()
                    .find(|t| t.name == "default")
                    .map(|t| t.template),
            })
            .map(|t| ChatTemplate::new(t, tokenizer_config.bos_token, tokenizer_config.eos_token));

        // Inference limit with a semaphore
        let semaphore = Arc::new(Semaphore::new(max_concurrent_requests));

        // Backend health
        let backend_health = Arc::new(AtomicBool::new(backend.start_health()));

        // Initialize NVML
        let nvml = Nvml::init().expect("Failed to initialize NVML");

        Self {
            validation,
            backend: Arc::new(backend),
            chat_template,
            limit_concurrent_requests: semaphore,
            backend_health,
            nvml: Arc::new(nvml),
        }
    }

    /// Add a new request to the queue and return a stream of InferStreamResponse
    #[instrument(skip_all)]
    pub(crate) async fn generate_stream<'a>(
        &'a self,
        request: GenerateRequest,
    ) -> Result<
        (
            OwnedSemaphorePermit,
            u32, // input_length
            impl Stream<Item = Result<InferStreamResponse, InferError>> + 'a,
        ),
        InferError,
    > {
        // Get device and initial energy consumption
        let device = self.nvml.device_by_index(0).map_err(|e| InferError::EnergyConsumptionError(e.to_string()))?;
        let energy_start = device.total_energy_consumption().map_err(|e| InferError::EnergyConsumptionError(e.to_string()))?;
        println!("energy_start: {:?}", energy_start);

        // Limit concurrent requests by acquiring a permit from the semaphore
        let permit = self
            .clone()
            .limit_concurrent_requests
            .try_acquire_owned()
            .map_err(|err| {
                metrics::counter!("tgi_request_failure", "err" => "overloaded").increment(1);
                tracing::error!("{err}");
                err
            })?;

        // Validate request
        let mut local_request = request.clone();
        let valid_request = self.validation.validate(request).await.map_err(|err| {
            metrics::counter!("tgi_request_failure", "err" => "validation").increment(1);
            tracing::error!("{err}");
            err
        })?;

        let seed = valid_request.parameters.seed;
        local_request.parameters.seed = Some(seed);
        let input_length = valid_request.input_length;
        let max_total_new_tokens = valid_request.stopping_parameters.max_total_new_tokens;

        let mut generation_stream = self.backend.schedule(valid_request)?;

        // Wrap generation stream to update the backend health if the stream contains an error
        let final_stream = stream! {
            let mut total_generated_tokens = 0;
            let mut first_start = None;
            let mut first_queued = None;
            let mut all_generated_text: Option<GeneratedText> = None;
            let mut energy_consumption_results: Option<u64> = None;
            let mut energy_last: Option<u64> = Some(energy_start);
            while let Some(response) = generation_stream.next().await {
                let response = response.inspect_err(|_err| {
                    self.backend_health.store(false, Ordering::SeqCst);
                })?;

                match response {
                    InferStreamResponse::Prefill(_) => yield Ok(response),
                    InferStreamResponse::Intermediate { token, top_tokens, energy_consumption } => {
                        total_generated_tokens += 1;
                        // Get current energy consumption
                        let current_energy = device.total_energy_consumption()
                            .map_err(|e| InferError::EnergyConsumptionError(e.to_string()))?;

                        let token_energy = current_energy - energy_last.unwrap();
                        energy_last = Some(current_energy);
                        energy_consumption_results = Some(current_energy - energy_start);
                        println!("total_generated_tokens: {:?}", total_generated_tokens);
                        println!("token_energy: {:?}", token_energy);
                        println!("energy_consumption_results: {:?}", energy_consumption_results);
                        yield Ok(InferStreamResponse::Intermediate { 
                            token, 
                            top_tokens,
                            energy_consumption: energy_consumption_results,
                        });
                    }
                    InferStreamResponse::End { token, top_tokens,generated_text, start, queued, energy_consumption } => {
                        total_generated_tokens += 1;
                        first_start = first_start.or(Some(start));
                        first_queued = first_queued.or(Some(queued));
                        if let Some(v) = all_generated_text.as_mut() {
                            v.text.push_str(&generated_text.text);
                            v.generated_tokens = total_generated_tokens;
                            v.finish_reason = generated_text.finish_reason.clone();
                        };

                        if matches!(generated_text.finish_reason, FinishReason::Length) && total_generated_tokens < max_total_new_tokens {
                            local_request.inputs.push_str(&generated_text.text);
                            all_generated_text = all_generated_text.or(Some(generated_text));

                            let valid_request = match self.validation.validate(local_request.clone()).await {
                                Ok(valid_request) => valid_request,
                                Err(err) => {
                                    tracing::debug!("Failed to continue request: {err}");
                                    let energy_end = device.total_energy_consumption()
                                        .map_err(|e| InferError::GenerationError(e.to_string()))?;
                                    energy_consumption_results = Some(energy_end - energy_start);
                                    println!("energy_consumption_results: {:?}", energy_consumption_results);
                                    yield Ok(InferStreamResponse::End {token, top_tokens, generated_text: all_generated_text.unwrap(), start: first_start.unwrap(), queued: first_queued.unwrap(), energy_consumption: energy_consumption_results });
                                    break;
                                }
                            };

                            generation_stream = match self.backend.schedule(valid_request) {
                                Ok(stream) => {
                                    tracing::debug!("Continue request");
                                    println!("HERE: {:?}", energy_consumption);
                                    yield Ok(InferStreamResponse::Intermediate { token, top_tokens, energy_consumption,} );
                                    stream
                                },
                                Err(err) => {
                                    tracing::debug!("Failed to continue request: {err}");
                                    let energy_end = device.total_energy_consumption()
                                        .map_err(|e| InferError::GenerationError(e.to_string()))?;
                                    energy_consumption_results = Some(energy_end - energy_start);
                                    println!("energy_consumption_results: {:?}", energy_consumption_results);
                                    yield Ok(InferStreamResponse::End {token, top_tokens, generated_text: all_generated_text.unwrap(), start: first_start.unwrap(), queued: first_queued.unwrap(), energy_consumption: energy_consumption_results });
                                    break;
                                }
                            }
                        } else {
                            // Get final energy consumption
                            let energy_end = device.total_energy_consumption()
                                .map_err(|e| InferError::GenerationError(e.to_string()))?;
                            energy_consumption_results = Some(energy_end - energy_start);
                            println!("energy_consumption_results: {:?}", energy_consumption_results);
                            yield Ok(InferStreamResponse::End {
                                token,
                                top_tokens,
                                generated_text: all_generated_text.unwrap_or(generated_text),
                                start: first_start.unwrap(),
                                queued: first_queued.unwrap(),
                                energy_consumption: energy_consumption_results,
                            });
                            break;
                        }

                    }
                }
            }
        };

        Ok((permit, input_length, final_stream))
    }

    /// Tokenizer the input
    #[instrument(skip_all)]
    pub(crate) async fn tokenize(
        &self,
        request: GenerateRequest,
    ) -> Result<tokenizers::Encoding, InferError> {
        // Tokenize request
        let inputs = request.inputs;
        let add_special_tokens = request.add_special_tokens;
        let truncate = request.parameters.truncate;
        let encoding = self
            .validation
            .tokenize(inputs, add_special_tokens, truncate)
            .await
            .map_err(|err| {
                tracing::error!("Tokenization {err}");
                err
            })?;

        // Return Encoding
        Ok(encoding.0)
    }

    /// Apply the chat template to the chat request
    #[instrument(skip_all)]
    pub(crate) fn apply_chat_template(
        &self,
        messages: Vec<Message>,
        tools_and_prompt: Option<(Vec<Tool>, String)>,
    ) -> Result<String, InferError> {
        self.chat_template
            .as_ref()
            .ok_or_else(|| InferError::TemplateError(ErrorKind::TemplateNotFound.into()))?
            .apply(messages, tools_and_prompt)
            .map_err(|e| {
                metrics::counter!("tgi_request_failure", "err" => "template").increment(1);
                tracing::error!("{e}");
                e
            })
    }

    /// Add a new request to the queue and return a InferResponse
    #[instrument(skip_all)]
    pub(crate) async fn generate(
        &self,
        request: GenerateRequest,
    ) -> Result<InferResponse, InferError> {
        // Get device and initial energy consumption
        let device = self.nvml.device_by_index(0).map_err(|e| InferError::EnergyConsumptionError(e.to_string()))?;
        let energy_start = device.total_energy_consumption().map_err(|e| InferError::EnergyConsumptionError(e.to_string()))?;
        println!("energy_start: {:?}", energy_start);
        let use_top_tokens = request.parameters.top_n_tokens.is_some_and(|x| x > 0);

        // Create stream and keep semaphore permit as long as generate lives
        let (_permit, _input_length, stream) = self.generate_stream(request).await?;

        // Return values
        let mut result_prefill = Vec::new();
        let mut result_tokens = Vec::new();
        let mut result_top_tokens = Vec::new();
        let mut result_generated_text = None;
        let mut result_start = None;
        let mut result_queued = None;
        let mut result_energy_consumption = None;
        let mut result_token_energy_consumptions = Vec::new();

        let mut stream = Box::pin(stream);

        // Iterate on stream
        while let Some(response) = stream.next().await {
            match response? {
                // Add prefill tokens
                InferStreamResponse::Prefill(prefill_tokens) => {
                    result_prefill = prefill_tokens;
                }
                // Push last token
                InferStreamResponse::Intermediate { token, top_tokens, energy_consumption } => {
                    let mut token = token;
                    token.energy_consumption = energy_consumption;
                    result_tokens.push(token);
                    result_top_tokens.push(top_tokens);
                    result_token_energy_consumptions.push(energy_consumption);
                }
                // Final message
                // Set return values
                InferStreamResponse::End {
                    token,
                    generated_text,
                    start,
                    queued,
                    top_tokens,
                    energy_consumption,
                } => {
                    result_tokens.push(token);
                    result_top_tokens.push(top_tokens);
                    result_generated_text = Some(generated_text);
                    result_start = Some(start);
                    result_queued = Some(queued);
                    let energy_end = device.total_energy_consumption()
                        .map_err(|e| InferError::GenerationError(e.to_string()))?;
                    println!("energy_end: {:?}", energy_end);
                    result_energy_consumption = Some(energy_end - energy_start);
                    result_token_energy_consumptions.push(energy_consumption);
                }
            }
        }

        // Check that we received a `InferStreamResponse::End` message
        if let (Some(generated_text), Some(queued), Some(start)) =
            (result_generated_text, result_queued, result_start)
        {
            Ok(InferResponse {
                prefill: result_prefill,
                _input_length,
                tokens: result_tokens,
                generated_text,
                queued,
                start,
                top_tokens: if use_top_tokens {
                    result_top_tokens
                } else {
                    Vec::new()
                },
                energy_consumption: result_energy_consumption,
                token_energy_consumptions: result_token_energy_consumptions,
            })
        } else {
            let err = InferError::IncompleteGeneration;
            metrics::counter!("tgi_request_failure", "err" => "incomplete").increment(1);
            tracing::error!("{err}");
            Err(err)
        }
    }
    /// Add best_of new requests to the queue and return a InferResponse of the sequence with
    /// the highest log probability per token
    #[instrument(skip(self, request))]
    pub(crate) async fn generate_best_of(
        &self,
        request: GenerateRequest,
        best_of: usize,
    ) -> Result<(InferResponse, Vec<InferResponse>), InferError> {
        // validate  best_of parameter separately
        let best_of = self.validation.validate_best_of(best_of)?;

        // create multiple generate requests
        let mut infer_responses: Vec<InferResponse> =
            try_join_all((0..best_of).map(|_| self.generate(request.clone()))).await?;

        // get the sequence with the highest log probability per token
        let mut max_index = 0;
        let mut max_logprob: f32 = f32::MIN;

        for (i, response) in infer_responses.iter().enumerate() {
            // mean logprobs of the generated tokens
            let sequence_logprob = response
                .tokens
                .iter()
                .map(|token| token.logprob)
                .sum::<f32>()
                / response.tokens.len() as f32;

            // set best sequence
            if sequence_logprob > max_logprob {
                max_index = i;
                max_logprob = sequence_logprob;
            }
        }
        let best_response = infer_responses.remove(max_index);
        Ok((best_response, infer_responses))
    }

    #[instrument(skip(self))]
    pub(crate) async fn health(&self) -> bool {
        let health = self
            .backend
            .health(self.backend_health.load(Ordering::SeqCst))
            .await;
        self.backend_health.store(health, Ordering::SeqCst);
        health
    }
}

#[derive(Debug)]
pub struct GeneratedText {
    pub text: String,
    pub generated_tokens: u32,
    pub finish_reason: FinishReason,
    pub seed: Option<u64>,
}

#[derive(Debug)]
pub enum InferStreamResponse {
    // Optional first message
    Prefill(Vec<PrefillToken>),
    // Intermediate messages
    Intermediate {
        token: Token,
        top_tokens: Vec<Token>,
        energy_consumption: Option<u64>,
    },
    // Last message
    End {
        token: Token,
        top_tokens: Vec<Token>,
        generated_text: GeneratedText,
        start: Instant,
        queued: Instant,
        energy_consumption: Option<u64>,
    },
}

#[derive(Debug)]
pub(crate) struct InferResponse {
    /// input_length is the input as perceived by the rust tokenizer in the
    /// validation pathway. It is redundant with prefill.len() but prefill
    /// has data only if the user asked for it. This will always be filled.
    pub(crate) _input_length: u32,
    pub(crate) prefill: Vec<PrefillToken>,
    pub(crate) tokens: Vec<Token>,
    pub(crate) generated_text: GeneratedText,
    pub(crate) queued: Instant,
    pub(crate) start: Instant,
    pub(crate) top_tokens: Vec<Vec<Token>>,
    pub(crate) energy_consumption: Option<u64>,
    pub(crate) token_energy_consumptions: Vec<Option<u64>>,
}

#[derive(Debug, Error)]
pub enum InferError {
    #[error("Request failed during generation: {0}")]
    GenerationError(String),
    #[error("Model is overloaded")]
    Overloaded(#[from] TryAcquireError),
    #[error("Input validation error: {0}")]
    ValidationError(#[from] ValidationError),
    #[error("Incomplete generation")]
    IncompleteGeneration,
    #[error("Incomplete generation stream")]
    IncompleteGenerationStream,
    #[error("Template error: {0}")]
    TemplateError(#[from] minijinja::Error),
    #[error("Missing template vatiable: {0}")]
    MissingTemplateVariable(String),
    #[error("Tool error: {0}")]
    ToolError(String),
    #[error("Stream event serialization error")]
    StreamSerializationError(String),
    #[error("Energy consumption error: {0}")]
    EnergyConsumptionError(String),
}

impl InferError {
    pub(crate) fn error_type(&self) -> &str {
        match self {
            InferError::GenerationError(_) => "generation",
            InferError::Overloaded(_) => "overloaded",
            InferError::ValidationError(_) => "validation",
            InferError::IncompleteGeneration => "incomplete_generation",
            InferError::IncompleteGenerationStream => "incomplete_generation_stream",
            InferError::TemplateError(_) => "template_error",
            InferError::MissingTemplateVariable(_) => "missing_template_variable",
            InferError::ToolError(_) => "tool_error",
            InferError::StreamSerializationError(_) => "stream_serialization_error",
            InferError::EnergyConsumptionError(_) => "energy_consumption_error",
        }
    }

    pub(crate) fn into_openai_event(self) -> Event {
        Event::default()
            .json_data(OpenaiErrorEvent {
                error: APIError {
                    message: self.to_string(),
                    http_status_code: 422,
                },
            })
            .unwrap()
    }
}

#[derive(Serialize)]
pub struct APIError {
    message: String,
    http_status_code: usize,
}

#[derive(Serialize)]
pub struct OpenaiErrorEvent {
    error: APIError,
}
