#!/bin/sh
set -eu

cometbft unsafe-reset-all
cometbft start
