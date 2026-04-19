# Roadmap

This document tracks future releases and expansion areas without turning the main README into a status report.

## Runtime Hardening

- buffer or queue subscription-time notifications during WebSocket handshake
- add terminal transaction lifecycle ingestion so long-running active-tx counts stay tight
- expose structured logs and metrics instead of stdout-only effects

## State Coverage

- mirror initialized tick topology live instead of seeding it entirely from config
- add better reorg-aware state refresh semantics
- expand pool discovery beyond manual configuration

## Protocol Surface

- support additional router call shapes
- extend beyond exact-input-only analysis
- add broader venue and protocol coverage after the deterministic core hardens

## Product Surface

- add a stable CLI/config story for different local environments
- add persistence and replay tooling
- expose API and dashboard surfaces on top of the current runtime core
