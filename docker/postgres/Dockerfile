FROM postgres:18.7-bookworm

# Rebuilt package: the November 2026 PostgreSQL security update changes C headers.
ARG POSTGIS_VERSION=3.7.0+dfsg-1.pgdg12+2
RUN apt-get update \
    && apt-get install -y --no-install-recommends \
      postgresql-18-postgis-3=${POSTGIS_VERSION} \
      postgresql-18-postgis-3-scripts=${POSTGIS_VERSION} \
    && grep -aq DatumGetHeapTupleHeader /usr/lib/postgresql/18/lib/postgis-3.so \
    && rm -rf /var/lib/apt/lists/*
