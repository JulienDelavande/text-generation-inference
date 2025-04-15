#!/bin/bash
cd text-generation-inference
eval "$($HOME/miniconda/bin/conda shell.bash hook)"
conda activate text-generation-inference
conda install -c conda-forge pkg-config openssl
export OPENSSL_DIR=$CONDA_PREFIX && \
export OPENSSL_INCLUDE_DIR=$CONDA_PREFIX/include && \
export OPENSSL_LIB_DIR=$CONDA_PREFIX/lib && \
export PKG_CONFIG_PATH=$CONDA_PREFIX/lib/pkgconfig
export PYTHONPATH=/home/user/miniconda/envs/text-generation-inference/lib/python3.11/site-packages
ln -s /usr/lib/x86_64-linux-gnu/libnvidia-ml.so.1 /app/libnvidia-ml.so

LD_LIBRARY_PATH=/app:$LD_LIBRARY_PATH \
text-generation-launcher \
  --model-id HuggingFaceH4/zephyr-7b-beta \
  --disable-custom-kernels


text-generation-launcher \
  --model-id HuggingFaceH4/zephyr-7b-beta \
  --disable-custom-kernels

# To run the server in the background, use:
nohup text-generation-launcher \
  --model-id HuggingFaceH4/zephyr-7b-beta \
  --disable-custom-kernels > tgi.log 2>&1 &


# To stop the server, use:
ps aux | grep text-generation-launcher
pkill -f text-generation-launcher


