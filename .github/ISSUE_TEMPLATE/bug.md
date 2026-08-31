name: Bug 报告
description: 桌宠行为异常、状态错误、崩溃等问题
labels: ["bug"]
body:
  - type: markdown
    attributes:
      value: |
        感谢反馈！请尽量填写以下信息。
        **注意：不要粘贴包含个人路径、会话内容或其他敏感信息的完整日志。**

  - type: input
    id: version
    attributes:
      label: 桌宠版本
      description: 例如 0.3.13
      placeholder: "0.3.13"
    validations:
      required: true

  - type: input
    id: os
    attributes:
      label: Windows 版本
      placeholder: "Windows 11 23H2"
    validations:
      required: true

  - type: textarea
    id: tool
    attributes:
      label: 涉及的监测对象
      description: Claude Code / Codex / OpenCode / ZCode，及其版本（如已知）
      placeholder: "例如：Codex 桌面端"
    validations:
      required: true

  - type: textarea
    id: what-happened
    attributes:
      label: 发生了什么？
      description: 预期行为与实际行为的差异，出现时间点（任务开始/等待输入/完成/出错时？）
    validations:
      required: true

  - type: textarea
    id: repro
    attributes:
      label: 复现步骤
      placeholder: |
        1. ...
        2. ...

  - type: textarea
    id: logs
    attributes:
      label: 其他信息
      description: 脱敏后的截图或说明（可选）
