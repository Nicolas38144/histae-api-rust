FROM postgres:18.6-bookworm

ARG POSTGIS_VERSION=3.6.4+dfsg-2.pgdg12+1
RUN apt-get update \
    && apt-get install -y --no-install-recommends \
      postgresql-18-postgis-3=${POSTGIS_VERSION} \
      postgresql-18-postgis-3-scripts=${POSTGIS_VERSION} \
    && rm -rf /var/lib/apt/lists/*
