## Contributing to Orion

Thank you for your interest in contributing to **Orion**, a hybrid DAG + BFT consensus engine.  
This document describes how to set up your environment, run tests, and submit changes.

---

## Development Setup

- **Prerequisites**
  - Rust (stable toolchain, installed via [`rustup`](https://rustup.rs))
  - `cargo` available on your `PATH`

- **Clone the repository**

```bash
git clone git@github.com:YOUR_ORG/orion.git
cd orion
```

- **Build the project**

```bash
cargo build
```

---

## Running Tests

Please make sure tests pass before opening / updating a pull request.

```bash
# Run all tests
cargo test

# Run ordering guarantee tests explicitly
cargo test --test ordering_guarantees

# Run all tests in CI-equivalent mode (if you want to mirror CI locally)
cargo test --all --all-features
```

The GitHub Actions workflow (`.github/workflows/tests.yml`) automatically runs the test suite on all pushes and pull requests.

---

## Code Style & Quality

- **Rust style**
  - Follow standard Rust conventions and idioms.
  - Prefer small, well-named functions and clear control flow.

- **Format & Lint (recommended)**

```bash
cargo fmt
cargo clippy --all-targets --all-features -- -D warnings
```

Try to keep `clippy` warnings to zero where practical.

---

## Making Changes

1. **Create a feature branch**

```bash
git checkout -b feature/my-change
```

2. **Implement your change**
   - Keep commits logically grouped and with clear messages.
   - Add or update tests when changing behavior or adding features.

3. **Run tests locally**
   - At minimum: `cargo test`
   - For broader coverage: `cargo test --all --all-features`

4. **Update documentation (if needed)**
   - Keep `README.md` and any relevant comments up to date.

---

## Pull Requests

When opening a PR:

- **Describe the change**
  - What problem does it solve?
  - High-level summary of the approach.

- **Checklist**
  - [ ] Tests pass locally
  - [ ] New / changed behavior is covered by tests
  - [ ] Documentation is updated (if applicable)

Code review is a collaborative process—be open to feedback and discussion about design and trade-offs.

---

## Reporting Issues & Proposals

- **Bug reports**
  - Include steps to reproduce, expected vs. actual behavior, and environment details.

- **Feature requests / design proposals**
  - Explain the use case and how it fits into Orion’s goals (hybrid DAG + BFT consensus).
  - Be ready to discuss alternatives and trade-offs.

Thank you again for helping improve Orion!


