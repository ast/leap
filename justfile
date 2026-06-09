# leap — dev tasks (canon-cat branch). Run `just <recipe>`.

# The fileless workspace database lives under the XDG data dir.
data_dir := env_var_or_default('XDG_DATA_HOME', env_var('HOME') / '.local/share') / 'leap'

# List recipes.
default:
    @just --list

# Wipe the workspace database — ERASES ALL CONTENT. Next launch reseeds the tutorial.
reset:
    rm -f "{{data_dir}}/workspace.db" "{{data_dir}}/workspace.db-wal" "{{data_dir}}/workspace.db-shm"
    @echo "leap workspace wiped → next launch reseeds the tutorial"

# Print the workspace database path.
db-path:
    @echo "{{data_dir}}/workspace.db"

# Run the Wayland GUI (release); env forces Ubuntu's GPU stack over leaked Guix libs.
gui:
    env -u LIBRARY_PATH -u VDPAU_DRIVER_PATH XDG_DATA_DIRS=/usr/local/share:/usr/share:/var/lib/snapd/desktop cargo run --release --features gui --bin leap-gui

# Wipe the workspace, then launch the GUI fresh (handy while debugging).
gui-fresh: reset gui

# Run the terminal UI (release).
tui:
    cargo run --release

# Checks.
test:
    cargo test

clippy:
    cargo clippy --all-targets
    cargo clippy --features gui --bin leap-gui
