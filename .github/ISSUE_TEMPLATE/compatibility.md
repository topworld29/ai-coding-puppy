name: 兼容性问题
description: 某个工具更新后状态监测失效或误报
labels: ["compatibility"]
body:
  - type: markdown
    attributes:
      value: |
        小金毛桌宠依赖各工具的本地 Hook/数据格式，上游更新可能导致监测失效。
        反馈时请注明工具与版本，帮助我们跟进适配。

  - type: input
    id: tool
    attributes:
      label: 工具及版本
      placeholder: "例如：Claude Code 2.x / Codex 0.x / OpenCode x.x / ZCode x.x"
    validations:
      required: true

  - type: input
    id: pet-version
    attributes:
      label: 桌宠版本
      placeholder: "0.3.13"
    validations:
      required: true

  - type: dropdown
    id: symptom
    attributes:
      label: 症状
      options:
        - 完全不显示该工具的会话
        - 状态一直停在「运行中」
        - 任务完成后不提示
        - 没有等待输入却显示红字
        - 出现错误状态
        - 其他
    validations:
      required: true

  - type: textarea
    id: detail
    attributes:
      label: 详细说明
      description: 什么时间开始出现、该工具近期是否更新过版本、CLI 还是桌面端
    validations:
      required: true
