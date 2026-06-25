#!/usr/bin/env python3
"""Seed a directory with header-only GGUF files for TUI demos / recordings.

LlamaStash discovery and the GGUF parser only read the file header (magic,
counts, KV list, tensor info) and the on-disk size; they never read tensor
data. So a few hundred bytes of valid header, sparse-`truncate`d to a
realistic length, is enough to make a model show up in the catalog with a
believable name, arch, quant, mode, and size, without downloading real
weights. Byte layout mirrors `src/gguf/test_fixtures.rs`.

Each emitted file carries:
  - `general.architecture` / `general.name`
  - `general.file_type` so the Quant column shows a real label (15 = Q4_K_M)
  - `<arch>.*` shape keys (context_length, block_count, ...)
  - one `output_norm.weight` tensor so `infer_mode_hint()` reads it as a
    chat model instead of falling through to Embedding
and is sparse-sized so the Size column reads in GB, not bytes.

Pair with an isolated daemon so it never touches the user's real library:

    export LLAMASTASH_STATE_DIR=... LLAMASTASH_CONFIG_DIR=... \
           LLAMASTASH_CACHE_DIR=... HF_HOME=...
    export LLAMASTASH_MODEL_PATHS=/path/to/seeded LLAMASTASH_NO_SCAN=1
    python3 scripts/tui/seed_demo_models.py /path/to/seeded

`LLAMASTASH_NO_SCAN=1` skips the default HF/Ollama/LM Studio caches so only
the seeded directory is listed.

Usage: seed_demo_models.py <out_dir> [name=arch:general_name:ctx:ftype:size_gb ...]
With no model specs, writes the default curated trio (qwen2 / llama / gemma2).
"""
import os
import struct
import sys

# name, arch, general.name, context_length, general.file_type, size_bytes
DEFAULT_MODELS = [
    ("qwen2.5-coder-7b-instruct-q4_k_m.gguf", "qwen2", "Qwen2.5 Coder 7B Instruct", 32768, 15, 4_400_000_000),
    ("llama-3.1-8b-instruct-q4_k_m.gguf", "llama", "Llama 3.1 8B Instruct", 131072, 15, 4_920_000_000),
    ("gemma-2-9b-it-q4_k_m.gguf", "gemma2", "Gemma 2 9B Instruct", 8192, 15, 5_760_000_000),
]


def _w_str(b, s):
    enc = s.encode("utf-8")
    b += struct.pack("<Q", len(enc))
    b += enc


def _w_kv(b, key, ty, val):
    _w_str(b, key)
    if ty == "str":
        b += struct.pack("<I", 8)
        _w_str(b, val)
    elif ty == "u64":
        b += struct.pack("<I", 10)
        b += struct.pack("<Q", val)
    else:
        raise ValueError(ty)


def build_header(arch, name, ctx, file_type):
    meta = [
        ("general.architecture", "str", arch),
        ("general.name", "str", name),
        ("general.file_type", "u64", file_type),
        (f"{arch}.context_length", "u64", ctx),
        (f"{arch}.block_count", "u64", 32),
        (f"{arch}.embedding_length", "u64", 4096),
        (f"{arch}.attention.head_count", "u64", 32),
        (f"{arch}.attention.head_count_kv", "u64", 8),
        (f"{arch}.feed_forward_length", "u64", 14336),
        ("tokenizer.ggml.model", "str", "gpt2"),
    ]
    tensors = [("output_norm.weight", [4096], 0)]  # ggml_type 0 = F32 -> chat mode
    out = bytearray()
    out += b"GGUF"
    out += struct.pack("<I", 3)            # version
    out += struct.pack("<Q", len(tensors))
    out += struct.pack("<Q", len(meta))
    for key, ty, val in meta:
        _w_kv(out, key, ty, val)
    for tname, dims, gtype in tensors:
        _w_str(out, tname)
        out += struct.pack("<I", len(dims))
        for d in dims:
            out += struct.pack("<Q", d)
        out += struct.pack("<I", gtype)
        out += struct.pack("<Q", 0)        # offset
    return bytes(out)


def write_model(path, arch, name, ctx, file_type, size_bytes):
    header = build_header(arch, name, ctx, file_type)
    with open(path, "wb") as f:
        f.write(header)
        if size_bytes > len(header):
            f.truncate(size_bytes)         # sparse: realistic Size, ~0 disk use


def parse_spec(spec):
    fname, rest = spec.split("=", 1)
    arch, gname, ctx, ftype, size_gb = rest.split(":")
    return (fname, arch, gname, int(ctx), int(ftype), int(float(size_gb) * 1_000_000_000))


def main():
    if len(sys.argv) < 2:
        sys.exit(f"usage: {sys.argv[0]} <out_dir> [name=arch:general_name:ctx:ftype:size_gb ...]")
    out_dir = sys.argv[1]
    os.makedirs(out_dir, exist_ok=True)
    models = [parse_spec(s) for s in sys.argv[2:]] if len(sys.argv) > 2 else DEFAULT_MODELS
    for fname, arch, gname, ctx, ftype, size in models:
        path = os.path.join(out_dir, fname)
        write_model(path, arch, gname, ctx, ftype, size)
        print(f"seeded {fname}  ({arch}, {size / 1e9:.1f} GB sparse)")


if __name__ == "__main__":
    main()
