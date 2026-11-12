-- 003: privchat_search_tokens 线性化重写（P0-5 热行锁 convoy 修复）。
--
-- 旧实现是 plpgsql 循环里 `toks := toks || substr(...)`：数组拼接每次复制整个
-- 数组，单个长为 n 的连续字符段产生 O(n²) 次元素拷贝。4KB 纯字母数字载荷实测
-- 188ms/次，而该函数在 messages INSERT 的 GIN 索引表达式里、处于 channels
-- FOR UPDATE 行锁持有期间执行——直接抬高同 channel 全部提交的串行锁底。
--
-- 新实现用 regexp_matches WITH ORDINALITY + generate_series + string_agg 在
-- SQL 层一次完成，O(n)。输出与旧实现逐字节一致（同序、同分隔符、同
-- to_tsvector('simple', ...) 调用），因此：
--   * idx_privchat_messages_search_tokens 存量索引项保持有效，无需 REINDEX；
--   * 查询侧 search_tokens_tsquery（message_repo）与索引共用同一函数，
--     搜索语义不变。
-- 等价性验证：旧/新函数在 16k 条真实压测载荷 + 边界用例（NULL/空串/纯标点/
-- CJK/4000 字符截断/超长单段）上输出全部一致（2026-11-12，beta 库）。

CREATE OR REPLACE FUNCTION public.privchat_search_tokens(input text) RETURNS tsvector
    LANGUAGE sql IMMUTABLE PARALLEL SAFE
    AS $$
    SELECT CASE
        WHEN input IS NULL OR btrim(input) = '' THEN ''::tsvector
        ELSE coalesce(
            (SELECT to_tsvector('simple', string_agg(tok, ' ' ORDER BY ord, i))
             FROM (
                 SELECT CASE WHEN char_length(m.run_arr[1]) = 1 THEN m.run_arr[1]
                             ELSE substr(m.run_arr[1], i.i, 2) END AS tok,
                        m.ord,
                        i.i
                 FROM regexp_matches(
                          lower(left(input, 4000)),
                          '[0-9a-z一-鿿㐀-䶿぀-ヿ가-힣]+', 'g')
                      WITH ORDINALITY AS m(run_arr, ord),
                      LATERAL generate_series(
                          1, greatest(char_length(m.run_arr[1]) - 1, 1)) AS i(i)
             ) t(tok, ord, i)),
            ''::tsvector)
    END;
$$;