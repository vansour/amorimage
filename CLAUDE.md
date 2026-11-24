# CLAUDE.md

1. 前后端所有依赖与所有 workflows（GitHub Actions）均使用最新稳定版本。唯一例外：前端 typescript 固定维持 ^6.0.3，不升级到 7.x。
2. git 提交禁止出现任何有关 Claude Code 的内容（包括 Co-Authored-By、"Generated with" 等一切署名）。
3. 部署只使用 compose 这一种方式：前端构建用 node:26-trixie，Rust 构建用 rust:trixie，运行阶段用 debian:trixie-slim，镜像命名为 ghcr.io/vansour/amorimage。
