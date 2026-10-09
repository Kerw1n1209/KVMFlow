# 贡献规范

## 开源范围

本仓库包含桌面软件、硬件诊断工具、构建依赖、测试、品牌素材和公开文档。
官网、管理后台、云端运营代码、部署状态及内部工作记录不属于本仓库的发布范围。
不要使用 `git add -f` 将被忽略的这些内容加入提交。

凭据、签名私钥、证书私钥、真实用户记录、兑换码和未经脱敏的诊断文件不得提交。
示例配置使用占位值；回归测试使用虚构标识或脱敏后的采集数据。
检查图片和附件中的可见文字，文本扫描无法覆盖所有图片内容。

## 提交消息

使用 Conventional Commits：

```text
<type>(<optional-scope>): <summary>
```

类型和示例见 `.gitmessage`。摘要说明具体改动；需要时在正文解释原因或兼容影响。
提交模板不得预置 AI 或机器人协作者署名，保持 `.gitmessage` 为通用消息模板。

本地启用模板和消息格式检查：

```sh
git config commit.template .gitmessage
git config core.hooksPath .githooks
```

## 验证

根据改动运行相关检查：

```sh
cargo fmt --manifest-path sidecar/Cargo.toml --all --check
cargo test --manifest-path sidecar/Cargo.toml --workspace
cd desktop
npm ci
npm test
npm run test:rust
```

硬件探针测试见 `probes/README.md`。模拟测试通过后，涉及 USB/DDC 行为的改动仍需真机验证。

提交前检查暂存文件和敏感信息：

```sh
git status --short
git diff --cached --check
git diff --cached
gitleaks git --staged --redact=100 --ignore-gitleaks-allow
```

Gitleaks 需要单独安装。它用于检查凭据，公开范围、图片内容和诊断中的个人信息仍需人工审核。
不要把私钥放进源码、提交消息或日志。构建与更新签名说明见 `docs/building.md`
和 `docs/auto-updates.md`。
