# Fork 发布版本与首次发布基线

本 Fork 的应用版本从 `100.60.20` 开始。`package.json`、Tauri 配置、
`Cargo.toml`、`Cargo.lock` 和 release-please manifest 保持同步。

主版本 `100` 在 Windows MSI 的 `255` 上限之内，可继续使用现有 `.msi`
安装包和自动更新流程。

上游历史版本和 CHANGELOG 保留原来的 `0.x` 版本号。首次 Fork 发布的
`bootstrap-sha` 指向上游 `0.60.20` 的提交
`a1f26da0ac93b858f7ce61c10d5c22be6809c420`，避免缺少上游标签时扫描全部历史，
重新执行历史提交中的 `Release-As` 指令。

迁移提交使用一次性的 `Release-As: 100.60.20` footer 指定首次发布版本。
后续发布按 Conventional Commits 自动递增，不设置永久的 `release-as`。

首次发布缺少 `aio-coding-hub-v100.60.20` 标签时，变更日志检查使用配置的
`bootstrap-sha`，并验证该提交属于当前发布分支。后续发布必须能够读取
manifest 对应的标签；标签缺失时应检查远端标签和 checkout 配置。

提交并推送这些修改到 `main` 后，release-please 会更新现有 release PR。
确认 PR 版本为 `100.60.20` 且变更日志只包含基线之后的提交，再合并该 PR。
合并将触发安装包构建、签名和 Release 发布。
