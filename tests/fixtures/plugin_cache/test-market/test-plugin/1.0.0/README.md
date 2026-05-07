<p align="center">
  <img src="https://example.com/logo.png" width="120" />
</p>

<h1 align="center">test-plugin</h1>

<p align="center">
  <strong>fixture for Quiver plugin manifest enrichment tests</strong>
</p>

<p align="center">
  <a href="https://example.com"><img src="https://img.shields.io/badge/test-fixture-blue" alt="Test"></a>
  <a href="LICENSE"><img src="https://img.shields.io/github/license/example/test-plugin" alt="License"></a>
</p>

> A short blockquote that should be skipped by the README excerpter because it is not the main paragraph.

[![Stars](https://img.shields.io/github/stars/example/test-plugin?style=flat)](https://github.com/example/test-plugin/stargazers)

# test-plugin

This plugin coordinates multiple agents working in parallel on Claude Code tasks. It dispatches subtasks to specialised workers, collects results, and merges them into a single coherent response. Use it when you need to scale beyond a single agent for complex orchestration scenarios involving testing, automation, and code review.

## Installation

Run `claude plugin install test-plugin@test-market` to install. The plugin registers itself under `~/.claude/plugins/cache/test-market/test-plugin/1.0.0/`.

## Usage

After install, invoke via `/test-plugin <task>` from Claude Code.
