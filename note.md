# git
ssh-keygen -t ed25519 -C "julien.delavande@huggingface.co"
cat ~/.ssh/id_ed25519.pub

# conda env
curl -sLo ~/miniconda.sh https://repo.anaconda.com/miniconda/Miniconda3-latest-Linux-x86_64.sh
bash ~/miniconda.sh -b -p $HOME/miniconda
eval "$($HOME/miniconda/bin/conda shell.bash hook)"


# protoc
(.venv) user@r-jdelavande-dev-tgi-q4t2a30t-fb276-ml3cm:/app/text-generation-inference$ PROTOC_ZIP=protoc-21.12-linux-x86_64.zip
curl -OL https://github.com/protocolbuffers/protobuf/releases/download/v21.12/$PROTOC_ZIP
  % Total    % Received % Xferd  Average Speed   Time    Time     Time  Current
                                 Dload  Upload   Total   Spent    Left  Speed
  0     0    0     0    0     0      0      0 --:--:-- --:--:-- --:--:--     0
100 1548k  100 1548k    0     0  7694k      0 --:--:-- --:--:-- --:--:-- 7694k
(.venv) user@r-jdelavande-dev-tgi-q4t2a30t-fb276-ml3cm:/app/text-generation-inference$ python -m zipfile -e $PROTOC_ZIP $HOME/protoc
(.venv) user@r-jdelavande-dev-tgi-q4t2a30t-fb276-ml3cm:/app/text-generation-inference$ export PATH="$HOME/protoc/bin:$PATH"
(.venv) user@r-jdelavande-dev-tgi-q4t2a30t-fb276-ml3cm:/app/text-generation-inference$ protoc --version
bash: /home/user/protoc/bin/protoc: Permission denied
(.venv) user@r-jdelavande-dev-tgi-q4t2a30t-fb276-ml3cm:/app/text-generation-inference$ chmod +x $HOME/protoc/bin/protoc
(.venv) user@r-jdelavande-dev-tgi-q4t2a30t-fb276-ml3cm:/app/text-generation-inference$ protoc --version
libprotoc 3.21.12

te