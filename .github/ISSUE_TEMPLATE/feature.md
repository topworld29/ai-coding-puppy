name: 功能建议
description: 希望桌宠支持的新功能或改进
labels: ["enhancement"]
body:
  - type: textarea
    id: problem
    attributes:
      label: 你希望解决什么问题？
      description: 描述你遇到的使用场景，而不仅仅是想要的功能本身
    validations:
      required: true

  - type: textarea
    id: solution
    attributes:
      label: 你期望的解决方案
      description: 你希望它如何工作（可选）
