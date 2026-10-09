# Solana program

- Program id: `5niYYBAr9YzrFDJXzwyUJyUqvRFHEW8cTRyiT7Kgs4pi` (Solana mainnet)

## Verifiable build

```bash
solana-verify build --library-name jday
solana-verify get-executable-hash target/deploy/jday.so
solana-verify get-program-hash -um 5niYYBAr9YzrFDJXzwyUJyUqvRFHEW8cTRyiT7Kgs4pi
```

The two hashes must be equal. The same build runs in this repository's "Verifiable build" workflow.
