# ⚡ project_uwa (Universal Web API — Rust Edition)

> **A lightweight, low-overhead, asynchronous Rust bridge designed to expose local browser-based LLM sessions as standard OpenAI and Anthropic-compatible HTTP APIs.**

---

> !WARNING
> **DISCLAIMER & LEGAL NOTICE**  
> This project is created strictly for **educational, academic, and research purposes** as a proof-of-concept in browser automation, CDP (Chrome DevTools Protocol) interaction, and low-level HTTP reverse-proxying.  
> 
> - **No Harm Intended:** The authors do not encourage, condone, or support any activity that violates the Terms of Service (ToS) of any web service or AI provider.
> - **User Responsibility:** Users are solely responsible for compliance with applicable terms, rate limits, and policies of third-party platforms.
> - **Non-Profit & Open Source:** This software is provided "as is" under the MIT License, without warranty of any kind. Use it responsibly and at your own risk.

---

## 💡 Motivation: Why Rust?

Modern developer tools and web-bridging scripts are heavily dominated by Python. While convenient, Python introduces significant runtime overhead, high memory usage, high abstraction layers, and threading constraints due to the GIL (Global Interpreter Lock).

**`project_uwa` was built with a simple philosophy: *Because we can, and Rust makes it better.***

This project serves as a clean-room, high-performance Rust refactoring inspired by open-source browser-bridge concepts like [`universal-web-api`](https://github.com/lumingya/universal-web-api). By leveraging Rust's zero-cost abstractions, asynchronous runtime (`tokio`), fast web framework (`axum`), and low-level browser automation (`cdp`), this project aims to demonstrate:

* **Minimal Memory Footprint:** Running a lean local bridge without heavy runtime interpreters.
* **Blazing Fast I/O:** Efficient event-driven async networking and direct CDP WebSocket communication.
* **Type-Safe Architecture:** Strong compile-time guarantees across session management, parsing, and request handling.
* **Rust Ecosystem Advocacy:** Promoting native, memory-safe, and resource-efficient tooling for developer infrastructure.

---

## 🛠️ Architecture Overview

The bridge operates as a multi-layered local proxy:

1. **HTTP API Layer (`axum` + `tokio`):** Accepts standard `/v1/chat/completions` requests from clients (Cursor, Continue, OpenAI SDKs) and streams back SSE responses.
2. **Session & Tab Management (`dashmap` + `governor`):** Manages local tab pools, concurrency, and session isolation.
3. **CDP Automation Layer:** Communicates with local Chromium browser instances via Chrome DevTools Protocol (CDP) for DOM interaction and network event monitoring.
4. **Response Parsing & Normalization (`scraper` + `serde_json`):** Parses DOM streams and network payloads into standardized JSON / SSE chunks.

---
