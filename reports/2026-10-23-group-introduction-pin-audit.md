# 群介绍与置顶：当前源码审计及轻量收尾记录

日期：2026-10-23。范围仅 privchat-server / react / web / h5 / ui 的群介绍与群消息置顶。

## 结论

主体实现已经存在，不应重复开发。本轮简单缺口已修复；不能据此宣布完整跨端 UI 验收完成，也不宣布 GROUP_PROFILE_EDIT_SPEC 全部完成（群改名不是本轮群介绍收尾范围）。

未提交、未 push，未启动 Android 真机、scrcpy、iOS 模拟器或 Gradle 构建。未新增群公告、置顶上限、置顶系统消息或撤回清理策略，未扩展实验客户端。

## 基线与已有实现

启动时六仓库均为干净状态。HEAD：server `a0171f1`、react `7e1697d`、web `28b32e9`、h5 `dadfa42`、ui `50585e6`、docs `7f55d5d`。

- 群介绍：服务端 `group/group/update` 已在数据库事务内复核 Owner/Admin 与有效成员资格，持久化 name/description；省略保留、空串清除、同值不推进群版本。旧 settings 写入口仍为 owner-only，资料持久化与新服务实例重载已有测试源码。
- 群介绍读取：settings/get 已从群表取 description，客户端已有失效提示、页面进入和恢复刷新。Web/H5/App 已有独立介绍展示与编辑，不迁移 announcement。
- 置顶：服务端已有消息归属校验、Owner/Admin 操作权限、成员读列表及“管理员不能取消群主置顶”判断；已有频道提示，不是聊天消息。
- Web/H5 已使用共享 `useGroupProjectionRefresh`，页面进入、匹配提示、连接变化、前台恢复及可见页面 30 秒补读均有代码。
- App `MessagePage.kt` 已有多条列表、原消息上下文加载、本地关闭与前台补读；`ChatSettingsPage.kt` 已有全员查看介绍、Owner/Admin 编辑 gate。
- 上述是源码审计结果，不是线上部署、RPC 全链路或设备 UI 验收结论。

## 本轮改动

1. `src/rpc/message/pin.rs`、`src/rpc/message/pin_insert.sql`：旧逻辑重复置顶时数据库保留原记录，回包却使用本次调用人和当前时间。改为原子 upsert 返回数据库原置顶者和时间，多条与排序保持不变。冲突路径是无业务字段变更的 UPDATE；当前实测表无用户触发器，不推进群实体版本。
2. `scripts/test-pin-idempotency-db.mjs`：读取 handler 实际 include 的 SQL，在真实 PostgreSQL 的已迁移表上执行；验证重试、其他管理员重复置顶、多条、排序、重复删除及删除后重新置顶。事务回滚夹具；数据库 identity sequence 可能前进，不声称所有序列也会回滚。该测试不经过 RPC 鉴权或广播。
3. `privchat-web/src/features/chat/conversation-panel.tsx`：群会话切换时清空旧置顶与旧管理角色，避免新群读取失败时继续展示上一群数据。
4. `privchat-h5/src/features/chat/chat-screen.tsx`：切换时同样清空；补上角色查询 effect 的取消标志清理，旧群请求完成后不再回写当前群权限。
5. `privchat-h5/src/features/group/group-info-screen.tsx`：成员或资料尚未加载时不传入介绍编辑点击处理器，而非可点击后才早退。
6. React 刷新 hook 新增三项回归；H5 新增四项群介绍组件测试。
7. 仅在允许的 `MESSAGE_PIN_SPEC.md` 中校正 upsert 实现说明。未修改其他 spec；共享工作区中主任务的 call/location/sticker/索引及其他文档改动均保留、未触碰。

## 实际执行与结果

工作目录均为对应仓库。测试使用单 worker，不下载依赖、不创建大型构建。

| 检查 | 结果与证据边界 |
| --- | --- |
| server: `PRIVCHAT_TEST_DATABASE_URL=postgres://zoujiaqing@localhost/privchat_test node scripts/test-pin-idempotency-db.mjs` | PASS；真实 PostgreSQL，使用生产 SQL，夹具回滚；不等同 RPC handler/权限/广播测试 |
| react: `npm test -- --maxWorkers=1 --minWorkers=1 tests/hooks/use-group-projection-refresh.test.tsx tests/hooks/use-group-ops.test.tsx` | 21/21 PASS（刷新 4、群操作 17）；覆盖后台不轮询、恢复、并发提示合并、销毁后不补读、失败后重试、最新 callback 与未激活页面 |
| react 全套：`npm test -- --maxWorkers=1 --minWorkers=1` | 修改测试前 229/229 PASS；最终复跑 29 文件、232/232 PASS（10.63 秒） |
| h5: `npm test -- --maxWorkers=1 --minWorkers=1 src/features/group/__tests__/group-introduction.test.tsx src/features/chat/__tests__/conversation-title.test.ts` | 10/10 PASS；其中介绍 4 项：成员只读且不展示旧公告、owner/admin 各自清空、保存失败保留草稿并可重试 |
| react / web: `npm run typecheck` | 均 PASS |
| h5: `node_modules/.bin/tsc --noEmit --pretty false` | PASS |
| server: `rustfmt --edition 2021 --check src/rpc/message/pin.rs` | PASS；仅格式/语法解析，不是 Rust 编译通过 |
| 各涉及仓库 `git diff --check` | PASS |
| server: `cargo test --test group_role_authz_db_test -- --test-threads=1`（指定上述真实测试库） | 因触发全量依赖编译及磁盘限制而中止；没有得到测试执行结果，禁止记为通过 |

新增 React 测试曾因 happy-dom getter spy 恢复导致相邻用例栈溢出；改为局部属性覆盖并清理后，定向 21 项已复跑通过，不隐去首次失败。

本轮生成的 server target 编译缓存已清理，保留原有 `target/debug/privchat` 符号链接；清理后 target 为 4 KiB。没有删除主任务的共享产物。

## UI 验证边界

H5 测试运行于 happy-dom，挂载真实 GroupInfoScreen，但替换 Taro/NutUI、SDK hooks 与反馈组件；只能证明页面逻辑及传参，不能证明真实 NutUI 外观、浏览器布局、网络或服务端权限。

未启动浏览器做实际点击/截图；Web 仅类型检查与源码审计。App 仅源码审计，未重新编译或启动设备，未验证真实布局、键盘、多条操作表、跳转和恢复。现有 helper hook 测试不等于真实双端实时同步。

## 收尾判定与后续最小检查

**可以收尾：本轮策略审计、明确的小缺口与轻量测试补齐。不能标记：完整跨端功能验收通过。**

后续只需定向验收，不需要扩大到全端项目：

1. 有足够磁盘或现有可用 target 后，复跑 `group_role_authz_db_test`，确认真实服务层资料权限、持久化/no-op/空串与新实例重载；当前 SQL 测试不能替代它。
2. 在已有 Web/H5 运行环境中，Owner/Admin/Member 查看同一群介绍；修改及清空后检查刷新、另一个在线成员、断线恢复。服务端进程重启与新设备读取另留证。
3. 同一群置顶两条：Member 可查看/跳转、无编辑入口；Admin 不能取消 Owner 置顶；关闭置顶条不取消全群置顶；切群且新群读取失败不残留旧群数据。
4. 两客户端检验 pin/unpin 提示与漏提示补读，后台不轮询；原消息不在已载窗口时定位成功，不可达时给出失败提示。
5. App 在主任务允许的现有运行环境里补对应实屏检查，不在本轮因缺证启动大型构建。任何 iOS 模拟器自动化另先报告。

条数上限、是否系统消息、撤回/删除处置仍按 spec 未决项保留，不以本轮修复擅自冻结。

补充收尾：审计时发现 H5 使用 `Error.message`、Web 使用 `errorText` 直接呈现介绍保存失败的原文，此缺口已在本轮修复。两端复用既有语言目录，对 IM 的权限、网络／超时和其他保存错误做局部映射；没有把仅支持 PLATFORM HTTP 的全站入口错误地套在 IM RPC 上，也没有新增错误码。Web 错误留在编辑弹窗，H5 保持草稿并可重试。补跑 H5 介绍测试 5/5、Web 错误映射测试 3/3、中英文及 Web i18n 检查、两端类型检查均通过。这些结果仍不代表实际浏览器／App 或服务端完整权限链路验收。

## 完整改动文件清单

- `privchat-server/src/rpc/message/pin.rs`
- `privchat-server/src/rpc/message/pin_insert.sql`
- `privchat-server/scripts/test-pin-idempotency-db.mjs`
- `privchat-server/reports/2026-10-23-group-introduction-pin-audit.md`
- `privchat-react/tests/hooks/use-group-projection-refresh.test.tsx`
- `privchat-web/src/features/chat/conversation-panel.tsx`
- `privchat-web/src/features/chat/group-info-dialog.tsx`
- `privchat-web/src/features/chat/group-introduction-error.ts`
- `privchat-web/tests/unit/group-introduction-error.spec.ts`
- `privchat-web/tests/unit/playwright.config.ts`
- `privchat-h5/src/features/chat/chat-screen.tsx`
- `privchat-h5/src/features/group/group-info-screen.tsx`
- `privchat-h5/src/features/group/group-introduction-error.ts`
- `privchat-h5/src/features/group/__tests__/group-introduction.test.tsx`
- `privchat-docs/spec/features/message/MESSAGE_PIN_SPEC.md`

`privchat-ui` 本轮仅审计，无本人代码改动；其他任务的改动不列入上述清单。
