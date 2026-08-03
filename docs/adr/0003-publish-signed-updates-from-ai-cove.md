# 从 AI Cove 发布 Turbo 签名更新

AI Cove Turbo 的正式更新清单固定发布到 `https://ai-cove.com/downloads/turbo/latest.json`，安装包、更新包和签名位于同一静态目录。GitHub Actions 负责在 macOS 与 Windows 原生 Runner 上构建、校验并合并发布产物，但 Actions Artifacts 只作为构建交付和备份，不作为客户端正式更新源。
