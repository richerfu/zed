# OHOS Hello Example - Building with ohrs

This example demonstrates how to use GPUI on OpenHarmony OS (OHOS) and how to build it with `ohrs build`.

## Important Notes

⚠️ **This example code is for reference only.** To use it in a real OHOS project, you need to:

1. Create a separate OHOS project using `ohrs init`
2. Copy the code to `src/lib.rs` (not `main.rs` or `examples/`)
3. Configure the project as a library with `crate-type = ["cdylib"]`

## Creating an OHOS Project with GPUI

### Step 1: Initialize OHOS Project

```bash
ohrs init my_gpui_app
cd my_gpui_app
```

### Step 2: Add Dependencies

Add to `Cargo.toml`:

```toml
[lib]
crate-type = ["cdylib"]

[dependencies]
gpui = { path = "../../zed/crates/gpui" }
openharmony-ability = { path = "../../openharmony-ability/crates/ability" }
openharmony-ability-derive = { path = "../../openharmony-ability/crates/derive" }
napi-ohos = "1.1"
napi-derive-ohos = "1.1"

[build-dependencies]
napi-build-ohos = "1.1"
```

### Step 3: Create build.rs

Create `build.rs` in the project root:

```rust
fn main() {
    napi_build_ohos::setup();
}
```

### Step 4: Copy Example Code

Copy the code from `examples/ohos_hello.rs` to `src/lib.rs` in your OHOS project.

### Step 5: Build

```bash
ohrs build
```

Or with cargo directly:

```bash
cargo build --target aarch64-unknown-linux-ohos --release
```

## Project Structure

Your OHOS project should look like this:

```
my_gpui_app/
├── Cargo.toml          # With [lib] crate-type = ["cdylib"]
├── build.rs            # Calls napi_build_ohos::setup()
├── src/
│   └── lib.rs          # Contains #[ability] function
└── ...
```

## Key Requirements

1. **Library Type**: Must be `cdylib`, not a binary
2. **Entry Point**: Use `#[ability]` macro, not `main()`
3. **Build Script**: Must call `napi_build_ohos::setup()`
4. **Dependencies**: Need `napi-ohos`, `napi-derive-ohos`, `napi-build-ohos`

## Troubleshooting

If `ohrs build` fails:

1. Ensure `crate-type = ["cdylib"]` is set in `Cargo.toml`
2. Check that `build.rs` exists and calls `napi_build_ohos::setup()`
3. Verify all napi dependencies are correctly specified
4. Make sure the code is in `src/lib.rs`, not `src/main.rs`
