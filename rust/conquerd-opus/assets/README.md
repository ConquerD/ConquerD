# conquerd-opus assets

This directory is intentionally mostly empty.

## DNN model data files

The Opus DNN model weights (DRED encoder, OSCE decoder, LACE/NoLACE, FARGAN)
ship as **C source arrays** distributed separately from the xiph/opus git
repository.  They are fetched from the Xiph.Org media server as a self-
verifying tarball (the filename *is* the SHA-256 hash of the tarball).

**The C data files must be extracted into `rust/conquerd-opus/opus/dnn/`
before building the `dnn` feature** (which is the default).  cmake compiles
them into libopus as static C arrays — no binary blob, no runtime I/O.

### Fetch instructions (run from the repository root)

```powershell
# Windows
powershell -ExecutionPolicy Bypass -File scripts/fetch_opus_weights.ps1
```

```bash
# Linux / macOS
bash scripts/fetch_opus_weights.sh
```

The scripts are idempotent: they skip the download if the sentinel file
`opus/dnn/lace_data.c` is already present.

### Building without DNN

If you cannot access the Xiph media server (e.g., in an offline CI environment),
disable the `dnn` feature:

```toml
# In the consumer crate's Cargo.toml
conquerd-opus = { path = "../conquerd-opus", default-features = false }
```

DRED and OSCE will be inactive; libopus builds without the data C files.
