#!/bin/bash

./create-lnd-ca.sh

echo "LOCAL_UID=$(id -u)" > .env
echo "LOCAL_GID=$(id -g)" >> .env
docker compose up -d
