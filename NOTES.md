# Installing rustup in container
See: [Issue](https://github.com/rust-lang/rustup/issues/2040)
Solution `curl https://sh.rustup.rs -sSf | bash -s -- -y --no-modify-path`
`. "$HOME/.cargo/env"`
`rustup default nightly`
`rustup update`

# error: could not find system library 'libudev' required by the 'hidapi' crate
``sudo apt install -y pkg-config libusb-1.0-0-dev libftdi1-dev
sudo apt-get install libudev-dev``