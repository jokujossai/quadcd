# Contributing to QuadCD

## Getting Started

1. Fork the repository
2. Clone your fork and create a branch for your change
3. Make your changes and ensure checks pass
4. Submit a pull request

## Development

### Prerequisites

- Rust (stable toolchain)
- Git
- Podman for containerized integration tests

### Building

```sh
cargo build
```

### Running Checks

Before submitting a PR, run:

```sh
cargo fmt --check
cargo clippy -- -D warnings
cargo test
```

CI enforces all three — formatting, lints, and tests must pass.

### Integration Tests

Most development changes only need the standard checks above. The containerized integration tests under `tests/containerized/` are slower, require Podman, and run inside a systemd-enabled container:

```sh
podman build -t quadcd-test -f tests/containerized/image/Containerfile .
podman run --rm -it --privileged --systemd=always quadcd-test
```

Run the containerized suite when you change behavior that depends on real systemd, Podman, generators, or service-mode integration.

## Pull Requests

- Keep PRs focused on a single change
- Include tests for new functionality
- Follow existing code style and patterns
