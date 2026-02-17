## 变更说明

<!-- 简要描述本 PR 的改动内容和目的 -->

## 变更类型

- [ ] Bug 修复
- [ ] 新功能
- [ ] 性能优化
- [ ] 重构
- [ ] 配置变更
- [ ] 文档

## 稳定性检查（STABILITY_SPEC）

> 参考 [STABILITY_SPEC Code Review Checklist](../privchat-docs/spec/01-global/STABILITY_SPEC.md)

**资源边界（禁令 1-3）**
- [ ] 是否引入了 `unbounded_channel`？如果是，是否满足豁免条件？
- [ ] 是否有新的 `tokio::spawn` 不受 semaphore/worker pool 约束？
- [ ] 是否有硬编码的连接池参数？

**可观测性（禁令 4-5）**
- [ ] 新增的队列/channel 是否有 depth/drop/fail 指标？
- [ ] 队列满时的降级路径是否定义了可丢/不可丢语义？

**缓存与并发**
- [ ] 新增的缓存是否有 TTL + size 上限 + 命中率监控？
- [ ] 新增的 `DashMap` 是否有可靠的 remove（drop guard）？

**不可阻塞路径（第 7 节）**
- [ ] 是否在不可阻塞路径（accept/read loop/heartbeat/SDK 主循环）中引入了 DB/Redis 同步 I/O？

**追踪（第 8 节）**
- [ ] Must-Deliver 任务是否携带 trace_id 并在关键节点记录？

## 测试

- [ ] 单元测试通过
- [ ] 集成测试通过（如适用）
- [ ] 压测验证通过（如涉及性能路径）

## 压测相关（如涉及）

- [ ] 已运行 `scripts/pre-loadtest-check.sh` 且全部通过
