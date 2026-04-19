# Lessons

- Prefer a compact single-crate structure until module boundaries harden. Avoid premature multi-crate expansion or file sprawl.
- Keep the README product-facing. Move roadmap items, limitations, and future-release notes into docs instead of framing the main project page as incomplete.
- Once the crate grows beyond a handful of source files, group code by concern and add tracked `tests/`, `configs/`, and `scripts/` before continuing feature work.
