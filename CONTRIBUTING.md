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

For changes that depend on real systemd or Podman, run the containerized suite (needs Podman):

```sh
./scripts/run-containerized-tests.sh
```

## Pull Requests

- Keep PRs focused on a single change
- Include tests for new functionality
- Follow existing code style and patterns
- Split files into logical submodules once they grow large
