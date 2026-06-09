FROM rust:1.95-bookworm

RUN apt-get update \
  && apt-get install -y --no-install-recommends \
    ca-certificates \
    curl \
    git \
    python3 \
    python3-venv \
    pkg-config \
    libssl-dev \
  && rm -rf /var/lib/apt/lists/*

ARG ORT_VERSION=1.24.4
RUN set -eux; \
  arch="$(uname -m)"; \
  case "$arch" in \
    x86_64|amd64) ort_arch="x64" ;; \
    aarch64|arm64) ort_arch="aarch64" ;; \
    *) echo "Unsupported ONNX Runtime arch: $arch" >&2; exit 1 ;; \
  esac; \
  url="https://github.com/microsoft/onnxruntime/releases/download/v${ORT_VERSION}/onnxruntime-linux-${ort_arch}-${ORT_VERSION}.tgz"; \
  mkdir -p /opt/onnxruntime; \
  curl -fsSL "$url" -o /tmp/onnxruntime.tgz; \
  tar -xzf /tmp/onnxruntime.tgz -C /opt/onnxruntime --strip-components=1; \
  rm /tmp/onnxruntime.tgz
ENV ORT_DYLIB_PATH="/opt/onnxruntime/lib/libonnxruntime.so"
ENV LD_LIBRARY_PATH="/opt/onnxruntime/lib"

WORKDIR /workspace
COPY . /workspace

RUN cargo build --release -p agent-file-tools

WORKDIR /workspace/benchmarks/aft-search
RUN chmod +x scripts/run-docker.sh 2>/dev/null || true
ENTRYPOINT ["bash", "scripts/run-docker.sh"]
