FROM ubuntu:26.04

RUN apt-get update && apt-get install -y
RUN apt install -y libwebkit2gtk-4.1-dev \
  build-essential \
  curl \
  wget \
  file \
  libxdo-dev \
  libssl-dev \
  libayatana-appindicator3-dev \
  librsvg2-dev \
  pkg-config libudev-dev libasound2-dev libpcsclite-dev \
  git

RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y 
ENV PATH="/root/.cargo/bin:${PATH}"
RUN rustup update

RUN cargo install tauri-cli --version "^2.0.0" --locked && \
    cargo install create-tauri-app --locked

RUN apt-get install -y \
    libwayland-client0 \
    libwayland-egl1 \
    libwayland-cursor0

RUN apt-get install -y \
    clang
