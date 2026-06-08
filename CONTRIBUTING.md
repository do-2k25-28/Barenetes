# Contributing to Barenetes

Thank you for your interest in contributing! This document explains how to get involved, what we expect from contributions, and how the project is structured.

---

## Table of Contents

- [Code of Conduct](#code-of-conduct)
- [Ways to Contribute](#ways-to-contribute)
- [Development Setup](#development-setup)
- [Project Structure](#project-structure)
- [Workflow](#workflow)
- [Commit Convention](#commit-convention)
- [Pull Request Guidelines](#pull-request-guidelines)
- [Coding Standards](#coding-standards)

---

## Code of Conduct

Be respectful and constructive. We welcome contributors of all experience levels.

---

## Ways to Contribute

- **Report bugs** : open an [issue](https://github.com/do-2k25-28/Barenetes/issues) with a clear reproduction case
- **Suggest features** : open an issue tagged `enhancement` and describe the motivation
- **Fix bugs** : pick up any open issue tagged `bug` and submit a PR
- **Implement features** : check open issues tagged `help wanted` or `good first issue`
- **Improve documentation** : typos, unclear sections, missing examples

If you plan to work on something non-trivial, open an issue first so we can align before you invest time.

---

## Development Setup

### Prerequisites

- [Rust](https://rustup.rs) (edition 2026 / nightly toolchain)
- [protoc](https://grpc.io/docs/protoc-installation/) : Protocol Buffer compiler

### Clone and build

```bash
git clone https://github.com/do-2k25-28/Barenetes.git
cd Barenetes
cargo build
```

### Run tests

```bash
cargo test
```

### Check formatting and lints

```bash
cargo fmt --check
cargo clippy -- -D warnings
```

---

## Project Structure

```
Barenetes/
├── api/            # API server (kube-apiserver equivalent)
├── scheduler/      # Scheduler / reconciliator (kube-scheduler equivalent)
├── kubelet/        # Node agent (kubelet equivalent)
├── kubectl/        # CLI client (kubectl equivalent)
├── cni/            # Network plugin (CNI equivalent)
└── proto/          # Protobuf definitions
    ├── api/v1/
    ├── cni/v1/
    ├── kubectl/v1/
    ├── kubelet/v1/
    └── scheduler/v1/
```

Each component is a separate Cargo crate. Inter-component communication is exclusively via gRPC, using the `.proto` files under `proto/`.

---

## Workflow

1. **Fork** the repository and create a branch from `main`
2. Branch naming: `feat/<short-description>`, `fix/<short-description>`, `docs/<short-description>`
3. Make your changes, keeping commits atomic and focused
4. Ensure `cargo fmt`, `cargo clippy`, and `cargo test` all pass
5. Open a pull request against `main`

---

## Commit Convention

We follow [Conventional Commits](https://www.conventionalcommits.org/):

```
<type>(<scope>): <short description>
```

Common types:

| Type | When to use |
|---|---|
| `feat` | New feature or capability |
| `fix` | Bug fix |
| `refactor` | Code change that neither fixes a bug nor adds a feature |
| `docs` | Documentation only |
| `test` | Adding or fixing tests |
| `chore` | Build process, tooling, dependency updates |

Examples:

```
feat(scheduler): implement pod scheduling in scheduler crate
fix(kubelet): correct gRPC timeout handling in kubelet
proto: add NodeStatus message to kubelet.proto
```

---

## Pull Request Guidelines

- One feature or fix per PR : keep scope tight
- Reference the related issue in the PR description (`Closes #42`)
- PR title must follow the commit convention above
- All CI checks must pass before merge
- At least one maintainer approval is required

---

## Coding Standards

- **Format**: run `cargo fmt` before committing
- **Lints**: code must compile with `cargo clippy -- -D warnings`
- **Comments**: only add comments when the *why* is non-obvious : avoid restating what the code does
- **Error handling**: use `Result` and propagate errors explicitly; avoid `unwrap()` outside of tests
- **Proto changes**: if you modify or add `.proto` files, regenerate the bindings and include them in the same PR
