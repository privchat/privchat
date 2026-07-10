-- P0-2 客户端消息搜索（MESSAGE_HISTORY_AND_SEARCH spec §4/§8.2）。
--
-- 为什么不用 010 的 pg_trgm：trgm 是 trigram，中文两字查询词（"福寿"/"红包"）
-- 提取不出可用 trigram，GIN 退化为全表 recheck。V1 拍板：应用层语义的
-- CJK bigram + tsvector('simple') 表达式 GIN 索引，不引入 pgroonga/pg_bigm
-- 第三方扩展。分词逻辑只存在于本函数（IMMUTABLE），写入索引与查询侧共用，
-- 避免 Rust/SQL 两处分词漂移。
--
-- 分词规则：
--   * lower 后按 [0-9a-z + CJK + 假名 + 谚文] 连续段切 run；
--   * 每个 run 切 2-gram（长度 1 的 run 保留单字 token）；ASCII 同样 2-gram，
--     使 "wor" 能命中 "world"（子串语义，最终由查询侧 ILIKE recheck 保证精确）；
--   * 输入截断到前 4000 字符，限制超长消息的索引成本。
--
-- 查询侧用法（服务端 Rust）：
--   tokens 匹配:  privchat_search_tokens(content) @@ to_tsquery('simple', $tq)
--     其中 $tq = array_to_string(tsvector_to_array(privchat_search_tokens($query)), ' & ')
--   精确 recheck: AND content ILIKE $pattern ESCAPE '\'（GIN 先缩候选集，ILIKE 只跑小集合）
--
-- 生产提示：当前生产库 2026-07-06 新建、数据量极小，索引即时可建；
-- 未来大表重建同 010 需维护窗口。

CREATE OR REPLACE FUNCTION privchat_search_tokens(input text)
RETURNS tsvector
LANGUAGE plpgsql
IMMUTABLE
PARALLEL SAFE
AS $func$
DECLARE
    run  text;
    n    int;
    toks text[] := '{}';
BEGIN
    IF input IS NULL OR btrim(input) = '' THEN
        RETURN ''::tsvector;
    END IF;

    FOR run IN
        SELECT (regexp_matches(
                    lower(left(input, 4000)),
                    '[0-9a-z一-鿿㐀-䶿぀-ヿ가-힣]+',
                    'g'))[1]
    LOOP
        n := char_length(run);
        IF n = 1 THEN
            toks := toks || run;
        ELSE
            FOR i IN 1..(n - 1) LOOP
                toks := toks || substr(run, i, 2);
            END LOOP;
        END IF;
    END LOOP;

    IF array_length(toks, 1) IS NULL THEN
        RETURN ''::tsvector;
    END IF;

    RETURN to_tsvector('simple', array_to_string(toks, ' '));
END;
$func$;

-- 表达式索引：所有消息写入路径（wire send / sync submit / admin 注入 / 系统
-- 消息 / RP-12 资金卡片）自动覆盖，无需每个 insert 路径各自维护 search 列，
-- 也无需数据 backfill——CREATE INDEX 本身即完成存量索引。
CREATE INDEX IF NOT EXISTS idx_privchat_messages_search_tokens
    ON privchat_messages USING gin (privchat_search_tokens(content));
