FROM rust:1-slim-bookworm AS builder

WORKDIR /src/scraper

COPY backend/scraper/Cargo.toml backend/scraper/Cargo.lock ./
COPY backend/scraper/src ./src

RUN cargo build --release


FROM python:3.11-slim-bookworm

ARG BUILD_DATE
ARG VCS_REF

LABEL org.opencontainers.image.title="csearch-updater" \
      org.opencontainers.image.description="Congress data updater and scraper runtime" \
      org.opencontainers.image.source="https://registry.s8njee.com/csearch-updater" \
      org.opencontainers.image.created="${BUILD_DATE}" \
      org.opencontainers.image.revision="${VCS_REF}"

RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates \
    git \
    postgresql-client \
 && rm -rf /var/lib/apt/lists/*

COPY backend/scraper/requirements.txt /tmp/requirements.txt
RUN pip install --no-cache-dir -r /tmp/requirements.txt

RUN groupadd --gid 1000 csearch \
 && useradd --uid 1000 --gid 1000 --create-home --shell /usr/sbin/nologin csearch \
 && mkdir -p /srv/csearch /opt/csearch/congress \
 && chown -R csearch:csearch /srv/csearch

COPY --from=builder /src/scraper/target/release/csearch-rscraper /usr/local/bin/updater
COPY backend/scraper/congress/ /opt/csearch/congress/
COPY backend/scraper/docker-entrypoint.sh /usr/local/bin/docker-entrypoint.sh

RUN chmod +x /usr/local/bin/docker-entrypoint.sh

ENV CONGRESSDIR=/srv/csearch
ENV POSTGRESURI=postgres

WORKDIR /srv/csearch

USER 1000:1000

ENTRYPOINT ["/usr/local/bin/docker-entrypoint.sh"]
CMD ["/usr/local/bin/updater"]
