name := 'wmde-taskmgr'
appid := 'fun.wmde.taskmgr'

rootdir := ''
prefix := '/usr'
cargo-target-dir := env('CARGO_TARGET_DIR', 'target')

base-dir := absolute_path(clean(rootdir / prefix))
bin-dst := base-dir / 'bin' / name
data-dir := base-dir / 'share'

desktop-dst := data-dir / 'applications' / (appid + '.desktop')
icons-src := 'res/icons/hicolor'
icons-dst := data-dir / 'icons/hicolor'

# Default recipe which runs `just build-release`
[private]
default: build-release

# Compiles with debug profile
build-debug *args:
    cargo build {{args}}

# Compiles with release profile
build-release *args: (build-debug '--release' args)

# Runs `cargo clean`
clean:
    cargo clean

# Runs a clippy check
check *args:
    cargo clippy --all-features {{args}} -- -W clippy::pedantic

# Runs a clippy check with JSON message format
check-json: (check '--message-format=json')

# Runs the test suite
test *args:
    cargo test {{args}}

# Installs files
install:
    install -Dm0755 {{ cargo-target-dir / 'release' / name }} {{bin-dst}}
    install -Dm0644 res/{{appid}}.desktop {{desktop-dst}}
    for size in `ls {{icons-src}}`; do \
        install -Dm0644 "{{icons-src}}/$size/apps/{{appid}}.svg" "{{icons-dst}}/$size/apps/{{appid}}.svg"; \
    done
