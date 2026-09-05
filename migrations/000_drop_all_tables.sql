-- 开发环境重置：把整个 public schema 推倒重来。
--
-- 🔴 migrate 命令**不会**执行 `000_` 开头的文件（见 migrate.rs 的编译期扫描），
-- 它只能手工跑：`psql "$DATABASE_URL" -f migrations/000_drop_all_tables.sql`。
--
-- 以前这里是一份写死的 25 张表 DROP 清单，加了新表没人记得往里补，于是「重置」
-- 之后还剩一堆旧表，而基线又是无条件 CREATE——撞名失败还算好的。直接重建 schema
-- 就不会漏，也不用维护。
DROP SCHEMA public CASCADE;
CREATE SCHEMA public;
GRANT ALL ON SCHEMA public TO public;
