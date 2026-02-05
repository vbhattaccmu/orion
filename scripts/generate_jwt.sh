#!/bin/sh
# Generate a random 32-byte JWT secret in hex format for Engine API authentication.
# Idempotent: skips generation if the file already exists.

JWT_FILE="/secrets/jwt.hex"

if [ -f "$JWT_FILE" ]; then
    echo "JWT secret already exists at $JWT_FILE"
    exit 0
fi

# Generate 32 random bytes, hex-encode them (64 hex characters)
JWT_SECRET=$(od -An -tx1 -N32 /dev/urandom | tr -d ' \n')

echo -n "$JWT_SECRET" > "$JWT_FILE"
echo "Generated JWT secret at $JWT_FILE (${#JWT_SECRET} hex chars)"
