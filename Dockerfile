# Neolink Docker image build scripts
# Copyright (c) 2020 George Hilliard,
#                    Andrew King,
#                    Miroslav Šedivý
# SPDX-License-Identifier: AGPL-3.0-only

FROM docker.io/rust:slim-bookworm AS build
ARG TARGETPLATFORM

ENV DEBIAN_FRONTEND=noninteractive
ENV CARGO_BUILD_JOBS=6
WORKDIR /usr/local/src/neolink
COPY . /usr/local/src/neolink

# Build the main program or copy from artifact
#
# We prefer copying from artifact to reduce
# build time on the github runners
#
# Because of this though, during normal
# github runner ops we are not testing the
# docker to see if it will build from scratch
# so if it is failing please make a PR
#
# hadolint ignore=DL3008
RUN  echo "TARGETPLATFORM: ${TARGETPLATFORM}"; \
  if [ -f "${TARGETPLATFORM}/neolink" ]; then \
    echo "Restoring from artifact"; \
    mkdir -p /usr/local/src/neolink/target/release/; \
    cp "${TARGETPLATFORM}/neolink" "/usr/local/src/neolink/target/release/neolink"; \
  else \
    echo "Building from scratch"; \
    apt-get update && \
        apt-get upgrade -y && \
        apt-get install -y --no-install-recommends \
          build-essential \
          openssl \
          libssl-dev \
          ca-certificates \
          libgstrtspserver-1.0-dev \
          libgstreamer1.0-dev \
          libgtk2.0-dev \
          protobuf-compiler \
          libglib2.0-dev && \
        apt-get clean -y && rm -rf /var/lib/apt/lists/* ; \
    cargo build --release; \
  fi

# Create the release container. Match the base OS used to build
FROM debian:bookworm-slim
ARG TARGETPLATFORM
ARG REPO
ARG VERSION
ARG OWNER

LABEL description="An image for the neolink program which is a reolink camera to rtsp translator"
LABEL repository="$REPO"
LABEL version="$VERSION"
LABEL maintainer="$OWNER"

# hadolint ignore=DL3008
RUN apt-get update && \
    apt-get upgrade -y && \
    apt-get install -y --no-install-recommends \
        openssl \
        dnsutils \
        iputils-ping \
        ca-certificates \
        libgstrtspserver-1.0-0 \
        libgstreamer1.0-0 \
        gstreamer1.0-tools \
        gstreamer1.0-x \
        gstreamer1.0-plugins-base \
        gstreamer1.0-plugins-good \
        gstreamer1.0-plugins-bad \
        gstreamer1.0-libav && \
    apt-get clean -y && rm -rf /var/lib/apt/lists/*

COPY --from=build \
  /usr/local/src/neolink/target/release/neolink \
  /usr/local/bin/neolink
COPY docker/entrypoint.sh /entrypoint.sh

RUN gst-inspect-1.0; \
    chmod +x "/usr/local/bin/neolink" && \
    "/usr/local/bin/neolink" --version && \
    mkdir -m 0700 /root/.config/

ENV NEO_LINK_MODE="rtsp" NEO_LINK_PORT=8554

CMD /usr/local/bin/neolink "${NEO_LINK_MODE}" --config /etc/neolink.toml
ENTRYPOINT ["/entrypoint.sh"]
EXPOSE ${NEO_LINK_PORT}

# Report unhealthy if the bridge is up but no longer serving frames (a frozen bridge
# keeps the process "running" and the RTSP port open, so only pulling a frame catches it).
# Many-camera setups may need a longer --timeout or can check a single camera.
HEALTHCHECK --interval=60s --timeout=30s --start-period=120s --retries=3 \
  CMD /usr/local/bin/neolink healthcheck --config /etc/neolink.toml

