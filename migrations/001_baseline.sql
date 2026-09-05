--
-- PrivChat server —— 1.0.0 beta1 合并基线。
--
-- 🔴 **这是给全新数据库用的基线，不是可以往存量库上补跑的迁移。**
--
-- 它由「跑完 001..027 全部历史迁移的库」`pg_dump --schema-only` 导出，所以结构
-- 与逐条跑历史迁移的结果逐字节一致（合并时用两个库对拍验证过）。历史那 27 个
-- 文件已删除：它们里有「先加列、后删列」这类互相抵消的步骤，留着只会让人以为
-- 存量库还能靠补跑追上来。
--
-- 存量库怎么办：本次发布不提供从旧结构原地升级的路径（Weey 生产已按此重建）。
-- runner 在库非空时会**拒绝**执行本基线，而不是装作成功——见 migrate.rs。
--
-- 为什么允许 pg_dump 风格（ATTACH PARTITION、写死的月分区）：原来的
-- 001_create_tables.sql 本身就是这么生成的，这里沿用同一套约定。
--

--
--


SET statement_timeout = 0;
SET lock_timeout = 0;
SET idle_in_transaction_session_timeout = 0;
SET client_encoding = 'UTF8';
SET standard_conforming_strings = on;
SET check_function_bodies = false;
SET xmloption = content;
SET client_min_messages = warning;
SET row_security = off;


--
-- Name: pg_trgm; Type: EXTENSION; Schema: -; Owner: -
--

CREATE EXTENSION IF NOT EXISTS pg_trgm WITH SCHEMA public;



--
-- Name: assign_privchat_channel_entity_sync_version(); Type: FUNCTION; Schema: public; Owner: -
--

CREATE FUNCTION public.assign_privchat_channel_entity_sync_version() RETURNS trigger
    LANGUAGE plpgsql
    AS $$
BEGIN
    NEW.sync_version = nextval('privchat_channel_entity_sync_version_seq');
    RETURN NEW;
END;
$$;



--
-- Name: assign_privchat_friend_sync_version(); Type: FUNCTION; Schema: public; Owner: -
--

CREATE FUNCTION public.assign_privchat_friend_sync_version() RETURNS trigger
    LANGUAGE plpgsql
    AS $$
BEGIN
    NEW.sync_version = nextval('privchat_friend_sync_version_seq');
    RETURN NEW;
END;
$$;



--
-- Name: assign_privchat_group_member_sync_version(); Type: FUNCTION; Schema: public; Owner: -
--

CREATE FUNCTION public.assign_privchat_group_member_sync_version() RETURNS trigger
    LANGUAGE plpgsql
    AS $$
BEGIN
    NEW.sync_version = nextval('privchat_group_member_sync_version_seq');
    RETURN NEW;
END;
$$;



--
-- Name: assign_privchat_group_sync_version(); Type: FUNCTION; Schema: public; Owner: -
--

CREATE FUNCTION public.assign_privchat_group_sync_version() RETURNS trigger
    LANGUAGE plpgsql
    AS $$
BEGIN
    NEW.sync_version = nextval('privchat_group_sync_version_seq');
    RETURN NEW;
END;
$$;



--
-- Name: assign_privchat_user_sync_version(); Type: FUNCTION; Schema: public; Owner: -
--

CREATE FUNCTION public.assign_privchat_user_sync_version() RETURNS trigger
    LANGUAGE plpgsql
    AS $$
BEGIN
    NEW.sync_version = nextval('privchat_user_entity_sync_version_seq');
    RETURN NEW;
END;
$$;



--
-- Name: now_millis(); Type: FUNCTION; Schema: public; Owner: -
--

CREATE FUNCTION public.now_millis() RETURNS bigint
    LANGUAGE sql IMMUTABLE
    AS $$
    SELECT (extract(epoch from now()) * 1000)::BIGINT;
$$;



--
-- Name: privchat_bump_group_membership_version(); Type: FUNCTION; Schema: public; Owner: -
--

CREATE FUNCTION public.privchat_bump_group_membership_version() RETURNS trigger
    LANGUAGE plpgsql
    AS $$
DECLARE
    target_group_id BIGINT;
BEGIN
    IF TG_OP = 'DELETE' THEN
        target_group_id := OLD.group_id;
    ELSE
        target_group_id := NEW.group_id;
    END IF;
    UPDATE privchat_channels
    SET membership_version = membership_version + 1
    WHERE channel_id = target_group_id AND channel_type = 1;
    IF TG_OP = 'DELETE' THEN
        RETURN OLD;
    END IF;
    RETURN NEW;
END;
$$;



--
-- Name: privchat_search_tokens(text); Type: FUNCTION; Schema: public; Owner: -
--

CREATE FUNCTION public.privchat_search_tokens(input text) RETURNS tsvector
    LANGUAGE plpgsql IMMUTABLE PARALLEL SAFE
    AS $$
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
$$;



--
-- Name: privchat_set_channel_read_cursor_sync_version(); Type: FUNCTION; Schema: public; Owner: -
--

CREATE FUNCTION public.privchat_set_channel_read_cursor_sync_version() RETURNS trigger
    LANGUAGE plpgsql
    AS $$
BEGIN
    NEW.sync_version := nextval('privchat_channel_read_cursor_sync_version_seq');
    RETURN NEW;
END;
$$;



--
-- Name: update_updated_at_column(); Type: FUNCTION; Schema: public; Owner: -
--

CREATE FUNCTION public.update_updated_at_column() RETURNS trigger
    LANGUAGE plpgsql
    AS $$
BEGIN
    NEW.updated_at = now_millis();
    RETURN NEW;
END;
$$;


SET default_tablespace = '';

SET default_table_access_method = heap;


--
-- Name: privchat_attachment_objects; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.privchat_attachment_objects (
    object_id bigint NOT NULL,
    plaintext_sha256 text NOT NULL,
    plaintext_size bigint NOT NULL,
    sealed_sha256 text NOT NULL,
    sealed_size bigint NOT NULL,
    file_path text NOT NULL,
    storage_source_id integer NOT NULL,
    format_version smallint NOT NULL,
    encryption_key_id smallint NOT NULL,
    published_at bigint DEFAULT public.now_millis() NOT NULL,
    CONSTRAINT privchat_attachment_objects_digests_are_sha256 CHECK (((plaintext_sha256 ~ '^[0-9a-f]{64}$'::text) AND (sealed_sha256 ~ '^[0-9a-f]{64}$'::text))),
    CONSTRAINT privchat_attachment_objects_format_version_is_current CHECK ((format_version = 1)),
    CONSTRAINT privchat_attachment_objects_key_id_in_range CHECK (((encryption_key_id >= 0) AND (encryption_key_id <= 255))),
    CONSTRAINT privchat_attachment_objects_sizes_are_sane CHECK (((plaintext_size >= 0) AND (sealed_size >= 0) AND (storage_source_id >= 0)))
);



--
-- Name: privchat_attachment_objects_object_id_seq; Type: SEQUENCE; Schema: public; Owner: -
--

CREATE SEQUENCE public.privchat_attachment_objects_object_id_seq
    START WITH 1
    INCREMENT BY 1
    NO MINVALUE
    NO MAXVALUE
    CACHE 1;



--
-- Name: privchat_attachment_objects_object_id_seq; Type: SEQUENCE OWNED BY; Schema: public; Owner: -
--

ALTER SEQUENCE public.privchat_attachment_objects_object_id_seq OWNED BY public.privchat_attachment_objects.object_id;



--
-- Name: privchat_blacklist; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.privchat_blacklist (
    user_id bigint NOT NULL,
    blocked_user_id bigint NOT NULL,
    created_at bigint DEFAULT public.now_millis() NOT NULL,
    reason character varying(256)
);



--
-- Name: privchat_bot_follow; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.privchat_bot_follow (
    id bigint NOT NULL,
    user_id bigint NOT NULL,
    bot_user_id bigint NOT NULL,
    channel_id bigint NOT NULL,
    status smallint DEFAULT 1 NOT NULL,
    followed_at bigint NOT NULL,
    unfollowed_at bigint,
    created_at bigint DEFAULT 0 NOT NULL,
    updated_at bigint DEFAULT 0 NOT NULL
);



--
-- Name: privchat_bot_follow_id_seq; Type: SEQUENCE; Schema: public; Owner: -
--

CREATE SEQUENCE public.privchat_bot_follow_id_seq
    START WITH 1
    INCREMENT BY 1
    NO MINVALUE
    NO MAXVALUE
    CACHE 1;



--
-- Name: privchat_bot_follow_id_seq; Type: SEQUENCE OWNED BY; Schema: public; Owner: -
--

ALTER SEQUENCE public.privchat_bot_follow_id_seq OWNED BY public.privchat_bot_follow.id;



--
-- Name: privchat_channel_entity_sync_version_seq; Type: SEQUENCE; Schema: public; Owner: -
--

CREATE SEQUENCE public.privchat_channel_entity_sync_version_seq
    START WITH 1
    INCREMENT BY 1
    NO MINVALUE
    NO MAXVALUE
    CACHE 1;



--
-- Name: privchat_channel_participants; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.privchat_channel_participants (
    channel_id bigint NOT NULL,
    user_id bigint NOT NULL,
    role smallint DEFAULT 2,
    nickname character varying(128),
    permissions jsonb DEFAULT '{}'::jsonb,
    mute_until bigint,
    joined_at bigint DEFAULT public.now_millis() NOT NULL,
    left_at bigint
);



--
-- Name: privchat_channel_pts; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.privchat_channel_pts (
    channel_id bigint NOT NULL,
    current_pts bigint DEFAULT 0 NOT NULL,
    created_at bigint DEFAULT public.now_millis() NOT NULL,
    updated_at bigint DEFAULT public.now_millis() NOT NULL
);



--
-- Name: privchat_channel_read_cursor; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.privchat_channel_read_cursor (
    user_id bigint NOT NULL,
    channel_id bigint NOT NULL,
    last_read_pts bigint NOT NULL,
    last_read_message_id bigint,
    updated_at timestamp with time zone DEFAULT CURRENT_TIMESTAMP NOT NULL,
    sync_version bigint NOT NULL
);



--
-- Name: privchat_channel_read_cursor_sync_version_seq; Type: SEQUENCE; Schema: public; Owner: -
--

CREATE SEQUENCE public.privchat_channel_read_cursor_sync_version_seq
    START WITH 1
    INCREMENT BY 1
    NO MINVALUE
    NO MAXVALUE
    CACHE 1;



--
-- Name: privchat_channels; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.privchat_channels (
    channel_id bigint NOT NULL,
    channel_type smallint NOT NULL,
    direct_user1_id bigint,
    direct_user2_id bigint,
    group_id bigint,
    last_message_id bigint,
    last_message_at bigint,
    message_count bigint DEFAULT 0,
    created_at bigint DEFAULT public.now_millis() NOT NULL,
    updated_at bigint DEFAULT public.now_millis() NOT NULL,
    create_source character varying(64),
    create_source_id character varying(256),
    sync_version bigint DEFAULT nextval('public.privchat_channel_entity_sync_version_seq'::regclass) NOT NULL,
    membership_version bigint DEFAULT 0 NOT NULL,
    server_latest_message_pts bigint,
    server_latest_message_id bigint,
    CONSTRAINT privchat_channels_check CHECK ((((channel_type = 0) AND (direct_user1_id IS NOT NULL) AND (direct_user2_id IS NOT NULL) AND (group_id IS NULL)) OR ((channel_type = 1) AND (group_id IS NOT NULL) AND (direct_user1_id IS NULL) AND (direct_user2_id IS NULL))))
);



--
-- Name: privchat_channels_channel_id_seq; Type: SEQUENCE; Schema: public; Owner: -
--

CREATE SEQUENCE public.privchat_channels_channel_id_seq
    START WITH 1
    INCREMENT BY 1
    NO MINVALUE
    NO MAXVALUE
    CACHE 1;



--
-- Name: privchat_channels_channel_id_seq; Type: SEQUENCE OWNED BY; Schema: public; Owner: -
--

ALTER SEQUENCE public.privchat_channels_channel_id_seq OWNED BY public.privchat_channels.channel_id;



--
-- Name: privchat_client_msg_registry; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.privchat_client_msg_registry (
    local_message_id bigint NOT NULL,
    server_msg_id bigint NOT NULL,
    pts bigint NOT NULL,
    channel_id bigint NOT NULL,
    channel_type smallint NOT NULL,
    sender_id bigint NOT NULL,
    decision character varying(20) DEFAULT 'accepted'::character varying NOT NULL,
    created_at bigint DEFAULT public.now_millis() NOT NULL,
    device_id character varying(128) DEFAULT ''::character varying NOT NULL
);



--
-- Name: privchat_commit_log; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.privchat_commit_log (
    id bigint NOT NULL,
    pts bigint NOT NULL,
    server_msg_id bigint NOT NULL,
    local_message_id bigint,
    channel_id bigint NOT NULL,
    channel_type smallint NOT NULL,
    message_type character varying(50) NOT NULL,
    content jsonb NOT NULL,
    server_timestamp bigint NOT NULL,
    sender_id bigint NOT NULL,
    sender_username character varying(100),
    created_at bigint DEFAULT public.now_millis() NOT NULL,
    event_schema_version smallint,
    canonical_event bytea
);



--
-- Name: privchat_commit_log_id_seq; Type: SEQUENCE; Schema: public; Owner: -
--

CREATE SEQUENCE public.privchat_commit_log_id_seq
    START WITH 1
    INCREMENT BY 1
    NO MINVALUE
    NO MAXVALUE
    CACHE 1;



--
-- Name: privchat_commit_log_id_seq; Type: SEQUENCE OWNED BY; Schema: public; Owner: -
--

ALTER SEQUENCE public.privchat_commit_log_id_seq OWNED BY public.privchat_commit_log.id;



--
-- Name: privchat_device_sync_state; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.privchat_device_sync_state (
    user_id bigint NOT NULL,
    device_id uuid NOT NULL,
    channel_id bigint NOT NULL,
    local_pts bigint DEFAULT 0,
    server_pts bigint DEFAULT 0,
    last_sync_at bigint DEFAULT public.now_millis() NOT NULL
);



--
-- Name: privchat_devices; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.privchat_devices (
    device_id uuid NOT NULL,
    user_id bigint NOT NULL,
    device_type character varying(32) NOT NULL,
    device_name character varying(128),
    device_model character varying(128),
    os_version character varying(64),
    app_version character varying(32),
    last_active_at bigint,
    created_at bigint DEFAULT public.now_millis() NOT NULL,
    updated_at bigint DEFAULT public.now_millis() NOT NULL,
    session_version bigint DEFAULT 1 NOT NULL,
    session_state smallint DEFAULT 0 NOT NULL,
    kicked_at bigint,
    kicked_by_device_id uuid,
    kicked_reason character varying(255),
    last_ip character varying(45)
);



--
-- Name: privchat_file_uploads; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.privchat_file_uploads (
    file_id bigint NOT NULL,
    original_filename character varying(512) NOT NULL,
    file_type character varying(32) NOT NULL,
    mime_type character varying(128) NOT NULL,
    object_id bigint NOT NULL,
    uploader_id bigint NOT NULL,
    uploader_ip character varying(45),
    uploaded_at bigint DEFAULT public.now_millis() NOT NULL,
    width integer,
    height integer,
    business_type character varying(64),
    business_id character varying(128),
    claim_key_hash character varying(64)
);



--
-- Name: privchat_file_uploads_id_seq; Type: SEQUENCE; Schema: public; Owner: -
--

CREATE SEQUENCE public.privchat_file_uploads_id_seq
    START WITH 1
    INCREMENT BY 1
    NO MINVALUE
    NO MAXVALUE
    CACHE 1;



--
-- Name: privchat_file_uploads_id_seq; Type: SEQUENCE OWNED BY; Schema: public; Owner: -
--

ALTER SEQUENCE public.privchat_file_uploads_id_seq OWNED BY public.privchat_file_uploads.file_id;



--
-- Name: privchat_friend_sync_version_seq; Type: SEQUENCE; Schema: public; Owner: -
--

CREATE SEQUENCE public.privchat_friend_sync_version_seq
    START WITH 1
    INCREMENT BY 1
    NO MINVALUE
    NO MAXVALUE
    CACHE 1;



--
-- Name: privchat_friendships; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.privchat_friendships (
    user_id bigint NOT NULL,
    friend_id bigint NOT NULL,
    status smallint DEFAULT 0,
    source character varying(64),
    source_id character varying(256),
    created_at bigint DEFAULT public.now_millis() NOT NULL,
    updated_at bigint DEFAULT public.now_millis() NOT NULL,
    request_message text,
    sync_version bigint DEFAULT nextval('public.privchat_friend_sync_version_seq'::regclass) NOT NULL,
    alias character varying(64),
    CONSTRAINT privchat_friendships_check CHECK ((user_id <> friend_id))
);



--
-- Name: privchat_group_join_requests; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.privchat_group_join_requests (
    request_id text NOT NULL,
    group_id bigint NOT NULL,
    user_id bigint NOT NULL,
    method_type text NOT NULL,
    method_ref text,
    status smallint DEFAULT 0 NOT NULL,
    message text,
    handler_id bigint,
    reject_reason text,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    updated_at timestamp with time zone DEFAULT now() NOT NULL,
    expires_at timestamp with time zone
);



--
-- Name: privchat_group_member_sync_version_seq; Type: SEQUENCE; Schema: public; Owner: -
--

CREATE SEQUENCE public.privchat_group_member_sync_version_seq
    START WITH 1
    INCREMENT BY 1
    NO MINVALUE
    NO MAXVALUE
    CACHE 1;



--
-- Name: privchat_group_members; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.privchat_group_members (
    group_id bigint NOT NULL,
    user_id bigint NOT NULL,
    role smallint DEFAULT 2,
    nickname character varying(128),
    permissions jsonb DEFAULT '{}'::jsonb,
    mute_until bigint,
    joined_at bigint DEFAULT public.now_millis() NOT NULL,
    left_at bigint,
    updated_at bigint DEFAULT public.now_millis() NOT NULL,
    sync_version bigint DEFAULT nextval('public.privchat_group_member_sync_version_seq'::regclass) NOT NULL
);



--
-- Name: privchat_group_pinned_messages; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.privchat_group_pinned_messages (
    id bigint NOT NULL,
    group_id bigint NOT NULL,
    message_id bigint NOT NULL,
    channel_id bigint NOT NULL,
    pinned_by bigint NOT NULL,
    pinned_at bigint DEFAULT public.now_millis() NOT NULL
);



--
-- Name: privchat_group_pinned_messages_id_seq; Type: SEQUENCE; Schema: public; Owner: -
--

ALTER TABLE public.privchat_group_pinned_messages ALTER COLUMN id ADD GENERATED ALWAYS AS IDENTITY (
    SEQUENCE NAME public.privchat_group_pinned_messages_id_seq
    START WITH 1
    INCREMENT BY 1
    NO MINVALUE
    NO MAXVALUE
    CACHE 1
);



--
-- Name: privchat_group_sync_version_seq; Type: SEQUENCE; Schema: public; Owner: -
--

CREATE SEQUENCE public.privchat_group_sync_version_seq
    START WITH 1
    INCREMENT BY 1
    NO MINVALUE
    NO MAXVALUE
    CACHE 1;



--
-- Name: privchat_groups; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.privchat_groups (
    group_id bigint DEFAULT nextval('public.privchat_channels_channel_id_seq'::regclass) NOT NULL,
    name character varying(128) NOT NULL,
    description text,
    avatar_url text,
    owner_id bigint NOT NULL,
    settings jsonb DEFAULT '{}'::jsonb,
    max_members integer DEFAULT 500,
    member_count integer DEFAULT 0,
    status smallint DEFAULT 0,
    created_at bigint DEFAULT public.now_millis() NOT NULL,
    updated_at bigint DEFAULT public.now_millis() NOT NULL,
    sync_version bigint DEFAULT nextval('public.privchat_group_sync_version_seq'::regclass) NOT NULL,
    qr_key character varying(16) NOT NULL,
    allow_search boolean DEFAULT true NOT NULL,
    join_policy smallint DEFAULT 1 NOT NULL,
    allow_member_invite boolean DEFAULT true NOT NULL,
    allow_member_add_friend boolean DEFAULT true NOT NULL,
    all_muted boolean DEFAULT false NOT NULL,
    forbid_forward boolean DEFAULT false NOT NULL,
    allow_member_post boolean DEFAULT true NOT NULL
);



--
-- Name: privchat_groups_group_id_seq; Type: SEQUENCE; Schema: public; Owner: -
--

CREATE SEQUENCE public.privchat_groups_group_id_seq
    START WITH 1
    INCREMENT BY 1
    NO MINVALUE
    NO MAXVALUE
    CACHE 1;



--
-- Name: privchat_groups_group_id_seq; Type: SEQUENCE OWNED BY; Schema: public; Owner: -
--

ALTER SEQUENCE public.privchat_groups_group_id_seq OWNED BY public.privchat_groups.group_id;



--
-- Name: privchat_login_logs; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.privchat_login_logs (
    log_id bigint NOT NULL,
    user_id bigint NOT NULL,
    device_id uuid NOT NULL,
    token_jti character varying(64) NOT NULL,
    token_created_at bigint NOT NULL,
    token_first_used_at bigint DEFAULT public.now_millis() NOT NULL,
    device_type character varying(32) NOT NULL,
    device_name character varying(128),
    device_model character varying(128),
    os_version character varying(64),
    app_id character varying(64) NOT NULL,
    app_version character varying(32),
    ip_address character varying(45) NOT NULL,
    user_agent text,
    login_method character varying(32) NOT NULL,
    auth_source character varying(64),
    country character varying(64),
    country_code character varying(3),
    region character varying(128),
    city character varying(128),
    latitude numeric(10,8),
    longitude numeric(11,8),
    timezone character varying(64),
    isp character varying(128),
    status smallint DEFAULT 0 NOT NULL,
    risk_score smallint DEFAULT 0 NOT NULL,
    risk_factors jsonb DEFAULT '[]'::jsonb,
    is_new_device boolean DEFAULT false NOT NULL,
    is_new_location boolean DEFAULT false NOT NULL,
    notification_sent boolean DEFAULT false NOT NULL,
    notification_sent_at bigint,
    notification_method character varying(32),
    metadata jsonb DEFAULT '{}'::jsonb,
    created_at bigint DEFAULT public.now_millis() NOT NULL
);



--
-- Name: privchat_login_logs_log_id_seq; Type: SEQUENCE; Schema: public; Owner: -
--

CREATE SEQUENCE public.privchat_login_logs_log_id_seq
    START WITH 1
    INCREMENT BY 1
    NO MINVALUE
    NO MAXVALUE
    CACHE 1;



--
-- Name: privchat_login_logs_log_id_seq; Type: SEQUENCE OWNED BY; Schema: public; Owner: -
--

ALTER SEQUENCE public.privchat_login_logs_log_id_seq OWNED BY public.privchat_login_logs.log_id;



--
-- Name: privchat_message_dedup; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.privchat_message_dedup (
    dedup_key text NOT NULL,
    message_id bigint NOT NULL,
    created_at bigint NOT NULL
);



--
-- Name: privchat_message_delivery_receipts; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.privchat_message_delivery_receipts (
    server_message_id bigint NOT NULL,
    receipt_type text NOT NULL,
    channel_id bigint NOT NULL,
    sender_id bigint NOT NULL,
    recipient_user_id bigint NOT NULL,
    ack_session_id bigint NOT NULL,
    delivered_at bigint NOT NULL,
    created_at bigint DEFAULT public.now_millis() NOT NULL
);



--
-- Name: privchat_message_dispatch_outbox; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.privchat_message_dispatch_outbox (
    event_id bigint NOT NULL,
    channel_id bigint NOT NULL,
    channel_type smallint NOT NULL,
    pts bigint NOT NULL,
    sender_id bigint NOT NULL,
    event_kind smallint NOT NULL,
    membership_version bigint NOT NULL,
    status smallint DEFAULT 0 NOT NULL,
    created_at bigint DEFAULT public.now_millis() NOT NULL,
    dispatched_at bigint,
    CONSTRAINT privchat_message_dispatch_outbox_event_kind_check CHECK ((event_kind = ANY (ARRAY[1, 2, 3]))),
    CONSTRAINT privchat_message_dispatch_outbox_status_check CHECK ((status = ANY (ARRAY[0, 1, 2])))
);



--
-- Name: privchat_message_dispatch_recipient; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.privchat_message_dispatch_recipient (
    event_id bigint NOT NULL,
    user_id bigint NOT NULL,
    state smallint DEFAULT 0 NOT NULL,
    lease_owner text,
    lease_until bigint,
    lease_token bigint DEFAULT 0 NOT NULL,
    attempts integer DEFAULT 0 NOT NULL,
    next_attempt_at bigint DEFAULT 0 NOT NULL,
    last_error text,
    CONSTRAINT privchat_message_dispatch_recipient_attempts_check CHECK ((attempts >= 0)),
    CONSTRAINT privchat_message_dispatch_recipient_state_check CHECK ((state = ANY (ARRAY[0, 1, 2, 3])))
);



--
-- Name: privchat_message_file_refs; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.privchat_message_file_refs (
    message_id bigint NOT NULL,
    message_created_at bigint NOT NULL,
    file_id bigint NOT NULL,
    role smallint NOT NULL,
    ordinal integer DEFAULT 0 NOT NULL,
    created_at bigint DEFAULT public.now_millis() NOT NULL
);



--
-- Name: privchat_message_reactions; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.privchat_message_reactions (
    message_id bigint NOT NULL,
    user_id bigint NOT NULL,
    emoji character varying(32) NOT NULL,
    created_at bigint DEFAULT ((EXTRACT(epoch FROM now()) * (1000)::numeric))::bigint NOT NULL,
    updated_at bigint DEFAULT ((EXTRACT(epoch FROM now()) * (1000)::numeric))::bigint NOT NULL
);



--
-- Name: privchat_messages; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.privchat_messages (
    message_id bigint NOT NULL,
    channel_id bigint NOT NULL,
    sender_id bigint NOT NULL,
    pts bigint NOT NULL,
    local_message_id bigint,
    message_type smallint NOT NULL,
    content text NOT NULL,
    metadata jsonb DEFAULT '{}'::jsonb,
    reply_to_message_id bigint,
    created_at bigint DEFAULT public.now_millis() NOT NULL,
    updated_at bigint DEFAULT public.now_millis() NOT NULL,
    deleted boolean DEFAULT false,
    deleted_at bigint,
    revoked boolean DEFAULT false,
    revoked_at bigint,
    revoked_by bigint
)
PARTITION BY RANGE (created_at);



--
-- Name: privchat_messages_2026_01; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.privchat_messages_2026_01 (
    message_id bigint NOT NULL,
    channel_id bigint NOT NULL,
    sender_id bigint NOT NULL,
    pts bigint NOT NULL,
    local_message_id bigint,
    message_type smallint NOT NULL,
    content text NOT NULL,
    metadata jsonb DEFAULT '{}'::jsonb,
    reply_to_message_id bigint,
    created_at bigint DEFAULT public.now_millis() NOT NULL,
    updated_at bigint DEFAULT public.now_millis() NOT NULL,
    deleted boolean DEFAULT false,
    deleted_at bigint,
    revoked boolean DEFAULT false,
    revoked_at bigint,
    revoked_by bigint
);



--
-- Name: privchat_messages_2026_02; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.privchat_messages_2026_02 (
    message_id bigint NOT NULL,
    channel_id bigint NOT NULL,
    sender_id bigint NOT NULL,
    pts bigint NOT NULL,
    local_message_id bigint,
    message_type smallint NOT NULL,
    content text NOT NULL,
    metadata jsonb DEFAULT '{}'::jsonb,
    reply_to_message_id bigint,
    created_at bigint DEFAULT public.now_millis() NOT NULL,
    updated_at bigint DEFAULT public.now_millis() NOT NULL,
    deleted boolean DEFAULT false,
    deleted_at bigint,
    revoked boolean DEFAULT false,
    revoked_at bigint,
    revoked_by bigint
);



--
-- Name: privchat_messages_2026_03; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.privchat_messages_2026_03 (
    message_id bigint NOT NULL,
    channel_id bigint NOT NULL,
    sender_id bigint NOT NULL,
    pts bigint NOT NULL,
    local_message_id bigint,
    message_type smallint NOT NULL,
    content text NOT NULL,
    metadata jsonb DEFAULT '{}'::jsonb,
    reply_to_message_id bigint,
    created_at bigint DEFAULT public.now_millis() NOT NULL,
    updated_at bigint DEFAULT public.now_millis() NOT NULL,
    deleted boolean DEFAULT false,
    deleted_at bigint,
    revoked boolean DEFAULT false,
    revoked_at bigint,
    revoked_by bigint
);



--
-- Name: privchat_messages_2026_04; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.privchat_messages_2026_04 (
    message_id bigint NOT NULL,
    channel_id bigint NOT NULL,
    sender_id bigint NOT NULL,
    pts bigint NOT NULL,
    local_message_id bigint,
    message_type smallint NOT NULL,
    content text NOT NULL,
    metadata jsonb DEFAULT '{}'::jsonb,
    reply_to_message_id bigint,
    created_at bigint DEFAULT public.now_millis() NOT NULL,
    updated_at bigint DEFAULT public.now_millis() NOT NULL,
    deleted boolean DEFAULT false,
    deleted_at bigint,
    revoked boolean DEFAULT false,
    revoked_at bigint,
    revoked_by bigint
);



--
-- Name: privchat_messages_default; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.privchat_messages_default (
    message_id bigint NOT NULL,
    channel_id bigint NOT NULL,
    sender_id bigint NOT NULL,
    pts bigint NOT NULL,
    local_message_id bigint,
    message_type smallint NOT NULL,
    content text NOT NULL,
    metadata jsonb DEFAULT '{}'::jsonb,
    reply_to_message_id bigint,
    created_at bigint DEFAULT public.now_millis() NOT NULL,
    updated_at bigint DEFAULT public.now_millis() NOT NULL,
    deleted boolean DEFAULT false,
    deleted_at bigint,
    revoked boolean DEFAULT false,
    revoked_at bigint,
    revoked_by bigint
);



--
-- Name: privchat_offline_message_queue; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.privchat_offline_message_queue (
    id bigint NOT NULL,
    user_id bigint NOT NULL,
    channel_id bigint NOT NULL,
    channel_type smallint NOT NULL,
    pts bigint NOT NULL,
    server_msg_id bigint NOT NULL,
    delivered smallint DEFAULT 0 NOT NULL,
    delivered_at bigint,
    created_at bigint DEFAULT public.now_millis() NOT NULL,
    expires_at bigint NOT NULL
);



--
-- Name: privchat_offline_message_queue_id_seq; Type: SEQUENCE; Schema: public; Owner: -
--

CREATE SEQUENCE public.privchat_offline_message_queue_id_seq
    START WITH 1
    INCREMENT BY 1
    NO MINVALUE
    NO MAXVALUE
    CACHE 1;



--
-- Name: privchat_offline_message_queue_id_seq; Type: SEQUENCE OWNED BY; Schema: public; Owner: -
--

ALTER SEQUENCE public.privchat_offline_message_queue_id_seq OWNED BY public.privchat_offline_message_queue.id;



--
-- Name: privchat_platform_settings; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.privchat_platform_settings (
    key text NOT NULL,
    value jsonb NOT NULL,
    updated_at bigint NOT NULL
);



--
-- Name: privchat_read_receipts; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.privchat_read_receipts (
    message_id bigint NOT NULL,
    user_id bigint NOT NULL,
    channel_id bigint NOT NULL,
    read_at bigint DEFAULT public.now_millis() NOT NULL
);



--
-- Name: privchat_refresh_tokens; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.privchat_refresh_tokens (
    jti character varying(64) NOT NULL,
    user_id bigint NOT NULL,
    device_id character varying(128) NOT NULL,
    token_hash character varying(64) NOT NULL,
    session_version bigint NOT NULL,
    expires_at bigint NOT NULL,
    revoked_at bigint,
    revoke_reason character varying(128),
    created_at bigint NOT NULL,
    last_used_at bigint
);



--
-- Name: privchat_user_channels; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.privchat_user_channels (
    user_id bigint NOT NULL,
    channel_id bigint NOT NULL,
    last_read_message_id bigint,
    last_read_at bigint,
    unread_count integer DEFAULT 0,
    is_pinned boolean DEFAULT false,
    is_muted boolean DEFAULT false,
    updated_at bigint DEFAULT public.now_millis() NOT NULL,
    sync_version bigint DEFAULT nextval('public.privchat_channel_entity_sync_version_seq'::regclass) NOT NULL,
    is_hidden boolean DEFAULT false NOT NULL
);



--
-- Name: privchat_user_devices; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.privchat_user_devices (
    id bigint NOT NULL,
    user_id bigint NOT NULL,
    device_id character varying(128) NOT NULL,
    platform character varying(32) NOT NULL,
    vendor character varying(32) NOT NULL,
    push_token text,
    apns_armed boolean DEFAULT false,
    connected boolean DEFAULT false,
    last_send_ts bigint,
    created_at timestamp without time zone DEFAULT now() NOT NULL,
    updated_at timestamp without time zone DEFAULT now() NOT NULL
);



--
-- Name: privchat_user_devices_id_seq; Type: SEQUENCE; Schema: public; Owner: -
--

CREATE SEQUENCE public.privchat_user_devices_id_seq
    START WITH 1
    INCREMENT BY 1
    NO MINVALUE
    NO MAXVALUE
    CACHE 1;



--
-- Name: privchat_user_devices_id_seq; Type: SEQUENCE OWNED BY; Schema: public; Owner: -
--

ALTER SEQUENCE public.privchat_user_devices_id_seq OWNED BY public.privchat_user_devices.id;



--
-- Name: privchat_user_entity_sync_version_seq; Type: SEQUENCE; Schema: public; Owner: -
--

CREATE SEQUENCE public.privchat_user_entity_sync_version_seq
    START WITH 1
    INCREMENT BY 1
    NO MINVALUE
    NO MAXVALUE
    CACHE 1;



--
-- Name: privchat_user_last_seen; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.privchat_user_last_seen (
    user_id bigint NOT NULL,
    last_seen_at bigint NOT NULL
);



--
-- Name: privchat_user_settings; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.privchat_user_settings (
    user_id bigint NOT NULL,
    setting_key character varying(128) NOT NULL,
    value_json jsonb DEFAULT '{}'::jsonb NOT NULL,
    version bigint DEFAULT 1 NOT NULL,
    updated_at bigint DEFAULT ((EXTRACT(epoch FROM now()) * (1000)::numeric))::bigint NOT NULL
);



--
-- Name: privchat_users; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.privchat_users (
    user_id bigint NOT NULL,
    username character varying(64),
    phone character varying(20),
    email character varying(255),
    password_hash character varying(255),
    display_name character varying(128),
    avatar_url text,
    user_type smallint DEFAULT 0,
    status smallint DEFAULT 0,
    privacy_settings jsonb DEFAULT '{}'::jsonb,
    created_at bigint DEFAULT public.now_millis() NOT NULL,
    updated_at bigint DEFAULT public.now_millis() NOT NULL,
    last_active_at bigint,
    sync_version bigint DEFAULT nextval('public.privchat_user_entity_sync_version_seq'::regclass) NOT NULL,
    business_system_id character varying(64),
    qr_key character varying(16) NOT NULL
);



--
-- Name: privchat_users_user_id_seq; Type: SEQUENCE; Schema: public; Owner: -
--

CREATE SEQUENCE public.privchat_users_user_id_seq
    START WITH 1
    INCREMENT BY 1
    NO MINVALUE
    NO MAXVALUE
    CACHE 1;



--
-- Name: privchat_users_user_id_seq; Type: SEQUENCE OWNED BY; Schema: public; Owner: -
--

ALTER SEQUENCE public.privchat_users_user_id_seq OWNED BY public.privchat_users.user_id;



--
-- Name: privchat_messages_2026_01; Type: TABLE ATTACH; Schema: public; Owner: -
--

ALTER TABLE ONLY public.privchat_messages ATTACH PARTITION public.privchat_messages_2026_01 FOR VALUES FROM ('1767196800000') TO ('1769875200000');



--
-- Name: privchat_messages_2026_02; Type: TABLE ATTACH; Schema: public; Owner: -
--

ALTER TABLE ONLY public.privchat_messages ATTACH PARTITION public.privchat_messages_2026_02 FOR VALUES FROM ('1769875200000') TO ('1772294400000');



--
-- Name: privchat_messages_2026_03; Type: TABLE ATTACH; Schema: public; Owner: -
--

ALTER TABLE ONLY public.privchat_messages ATTACH PARTITION public.privchat_messages_2026_03 FOR VALUES FROM ('1772294400000') TO ('1774972800000');



--
-- Name: privchat_messages_2026_04; Type: TABLE ATTACH; Schema: public; Owner: -
--

ALTER TABLE ONLY public.privchat_messages ATTACH PARTITION public.privchat_messages_2026_04 FOR VALUES FROM ('1774972800000') TO ('1777564800000');



--
-- Name: privchat_messages_default; Type: TABLE ATTACH; Schema: public; Owner: -
--

ALTER TABLE ONLY public.privchat_messages ATTACH PARTITION public.privchat_messages_default DEFAULT;



--
-- Name: privchat_attachment_objects object_id; Type: DEFAULT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.privchat_attachment_objects ALTER COLUMN object_id SET DEFAULT nextval('public.privchat_attachment_objects_object_id_seq'::regclass);



--
-- Name: privchat_bot_follow id; Type: DEFAULT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.privchat_bot_follow ALTER COLUMN id SET DEFAULT nextval('public.privchat_bot_follow_id_seq'::regclass);



--
-- Name: privchat_channels channel_id; Type: DEFAULT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.privchat_channels ALTER COLUMN channel_id SET DEFAULT nextval('public.privchat_channels_channel_id_seq'::regclass);



--
-- Name: privchat_commit_log id; Type: DEFAULT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.privchat_commit_log ALTER COLUMN id SET DEFAULT nextval('public.privchat_commit_log_id_seq'::regclass);



--
-- Name: privchat_file_uploads file_id; Type: DEFAULT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.privchat_file_uploads ALTER COLUMN file_id SET DEFAULT nextval('public.privchat_file_uploads_id_seq'::regclass);



--
-- Name: privchat_login_logs log_id; Type: DEFAULT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.privchat_login_logs ALTER COLUMN log_id SET DEFAULT nextval('public.privchat_login_logs_log_id_seq'::regclass);



--
-- Name: privchat_offline_message_queue id; Type: DEFAULT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.privchat_offline_message_queue ALTER COLUMN id SET DEFAULT nextval('public.privchat_offline_message_queue_id_seq'::regclass);



--
-- Name: privchat_user_devices id; Type: DEFAULT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.privchat_user_devices ALTER COLUMN id SET DEFAULT nextval('public.privchat_user_devices_id_seq'::regclass);



--
-- Name: privchat_users user_id; Type: DEFAULT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.privchat_users ALTER COLUMN user_id SET DEFAULT nextval('public.privchat_users_user_id_seq'::regclass);



--
-- Name: privchat_attachment_objects privchat_attachment_objects_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.privchat_attachment_objects
    ADD CONSTRAINT privchat_attachment_objects_pkey PRIMARY KEY (object_id);



--
-- Name: privchat_attachment_objects privchat_attachment_objects_plaintext_sha256_key; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.privchat_attachment_objects
    ADD CONSTRAINT privchat_attachment_objects_plaintext_sha256_key UNIQUE (plaintext_sha256);



--
-- Name: privchat_blacklist privchat_blacklist_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.privchat_blacklist
    ADD CONSTRAINT privchat_blacklist_pkey PRIMARY KEY (user_id, blocked_user_id);



--
-- Name: privchat_bot_follow privchat_bot_follow_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.privchat_bot_follow
    ADD CONSTRAINT privchat_bot_follow_pkey PRIMARY KEY (id);



--
-- Name: privchat_channel_participants privchat_channel_participants_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.privchat_channel_participants
    ADD CONSTRAINT privchat_channel_participants_pkey PRIMARY KEY (channel_id, user_id);



--
-- Name: privchat_channel_pts privchat_channel_pts_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.privchat_channel_pts
    ADD CONSTRAINT privchat_channel_pts_pkey PRIMARY KEY (channel_id);



--
-- Name: privchat_channel_read_cursor privchat_channel_read_cursor_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.privchat_channel_read_cursor
    ADD CONSTRAINT privchat_channel_read_cursor_pkey PRIMARY KEY (user_id, channel_id);



--
-- Name: privchat_channels privchat_channels_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.privchat_channels
    ADD CONSTRAINT privchat_channels_pkey PRIMARY KEY (channel_id);



--
-- Name: privchat_client_msg_registry privchat_client_msg_registry_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.privchat_client_msg_registry
    ADD CONSTRAINT privchat_client_msg_registry_pkey PRIMARY KEY (sender_id, device_id, local_message_id);



--
-- Name: privchat_commit_log privchat_commit_log_channel_id_pts_key; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.privchat_commit_log
    ADD CONSTRAINT privchat_commit_log_channel_id_pts_key UNIQUE (channel_id, pts);



--
-- Name: privchat_commit_log privchat_commit_log_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.privchat_commit_log
    ADD CONSTRAINT privchat_commit_log_pkey PRIMARY KEY (id);



--
-- Name: privchat_device_sync_state privchat_device_sync_state_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.privchat_device_sync_state
    ADD CONSTRAINT privchat_device_sync_state_pkey PRIMARY KEY (user_id, device_id, channel_id);



--
-- Name: privchat_devices privchat_devices_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.privchat_devices
    ADD CONSTRAINT privchat_devices_pkey PRIMARY KEY (user_id, device_id);



--
-- Name: privchat_devices privchat_devices_session_state_check; Type: CHECK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE public.privchat_devices
    ADD CONSTRAINT privchat_devices_session_state_check CHECK ((session_state = ANY (ARRAY[0, 1, 2, 3, 4]))) NOT VALID;



--
-- Name: privchat_file_uploads privchat_file_uploads_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.privchat_file_uploads
    ADD CONSTRAINT privchat_file_uploads_pkey PRIMARY KEY (file_id);



--
-- Name: privchat_friendships privchat_friendships_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.privchat_friendships
    ADD CONSTRAINT privchat_friendships_pkey PRIMARY KEY (user_id, friend_id);



--
-- Name: privchat_group_join_requests privchat_group_join_requests_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.privchat_group_join_requests
    ADD CONSTRAINT privchat_group_join_requests_pkey PRIMARY KEY (request_id);



--
-- Name: privchat_group_members privchat_group_members_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.privchat_group_members
    ADD CONSTRAINT privchat_group_members_pkey PRIMARY KEY (group_id, user_id);



--
-- Name: privchat_group_pinned_messages privchat_group_pinned_messages_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.privchat_group_pinned_messages
    ADD CONSTRAINT privchat_group_pinned_messages_pkey PRIMARY KEY (id);



--
-- Name: privchat_groups privchat_groups_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.privchat_groups
    ADD CONSTRAINT privchat_groups_pkey PRIMARY KEY (group_id);



--
-- Name: privchat_login_logs privchat_login_logs_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.privchat_login_logs
    ADD CONSTRAINT privchat_login_logs_pkey PRIMARY KEY (log_id);



--
-- Name: privchat_message_dedup privchat_message_dedup_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.privchat_message_dedup
    ADD CONSTRAINT privchat_message_dedup_pkey PRIMARY KEY (dedup_key);



--
-- Name: privchat_message_delivery_receipts privchat_message_delivery_receipts_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.privchat_message_delivery_receipts
    ADD CONSTRAINT privchat_message_delivery_receipts_pkey PRIMARY KEY (server_message_id, receipt_type);



--
-- Name: privchat_message_dispatch_outbox privchat_message_dispatch_outbox_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.privchat_message_dispatch_outbox
    ADD CONSTRAINT privchat_message_dispatch_outbox_pkey PRIMARY KEY (event_id);



--
-- Name: privchat_message_dispatch_recipient privchat_message_dispatch_recipient_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.privchat_message_dispatch_recipient
    ADD CONSTRAINT privchat_message_dispatch_recipient_pkey PRIMARY KEY (event_id, user_id);



--
-- Name: privchat_message_file_refs privchat_message_file_refs_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.privchat_message_file_refs
    ADD CONSTRAINT privchat_message_file_refs_pkey PRIMARY KEY (message_id, role, ordinal);



--
-- Name: privchat_message_reactions privchat_message_reactions_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.privchat_message_reactions
    ADD CONSTRAINT privchat_message_reactions_pkey PRIMARY KEY (message_id, user_id);



--
-- Name: privchat_messages privchat_messages_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.privchat_messages
    ADD CONSTRAINT privchat_messages_pkey PRIMARY KEY (message_id, created_at);



--
-- Name: privchat_messages_2026_01 privchat_messages_2026_01_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.privchat_messages_2026_01
    ADD CONSTRAINT privchat_messages_2026_01_pkey PRIMARY KEY (message_id, created_at);



--
-- Name: privchat_messages_2026_02 privchat_messages_2026_02_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.privchat_messages_2026_02
    ADD CONSTRAINT privchat_messages_2026_02_pkey PRIMARY KEY (message_id, created_at);



--
-- Name: privchat_messages_2026_03 privchat_messages_2026_03_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.privchat_messages_2026_03
    ADD CONSTRAINT privchat_messages_2026_03_pkey PRIMARY KEY (message_id, created_at);



--
-- Name: privchat_messages_2026_04 privchat_messages_2026_04_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.privchat_messages_2026_04
    ADD CONSTRAINT privchat_messages_2026_04_pkey PRIMARY KEY (message_id, created_at);



--
-- Name: privchat_messages_default privchat_messages_default_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.privchat_messages_default
    ADD CONSTRAINT privchat_messages_default_pkey PRIMARY KEY (message_id, created_at);



--
-- Name: privchat_offline_message_queue privchat_offline_message_queue_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.privchat_offline_message_queue
    ADD CONSTRAINT privchat_offline_message_queue_pkey PRIMARY KEY (id);



--
-- Name: privchat_platform_settings privchat_platform_settings_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.privchat_platform_settings
    ADD CONSTRAINT privchat_platform_settings_pkey PRIMARY KEY (key);



--
-- Name: privchat_read_receipts privchat_read_receipts_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.privchat_read_receipts
    ADD CONSTRAINT privchat_read_receipts_pkey PRIMARY KEY (message_id, user_id);



--
-- Name: privchat_refresh_tokens privchat_refresh_tokens_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.privchat_refresh_tokens
    ADD CONSTRAINT privchat_refresh_tokens_pkey PRIMARY KEY (jti);



--
-- Name: privchat_refresh_tokens privchat_refresh_tokens_token_hash_key; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.privchat_refresh_tokens
    ADD CONSTRAINT privchat_refresh_tokens_token_hash_key UNIQUE (token_hash);



--
-- Name: privchat_user_channels privchat_user_channels_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.privchat_user_channels
    ADD CONSTRAINT privchat_user_channels_pkey PRIMARY KEY (user_id, channel_id);



--
-- Name: privchat_user_devices privchat_user_devices_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.privchat_user_devices
    ADD CONSTRAINT privchat_user_devices_pkey PRIMARY KEY (id);



--
-- Name: privchat_user_devices privchat_user_devices_user_id_device_id_key; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.privchat_user_devices
    ADD CONSTRAINT privchat_user_devices_user_id_device_id_key UNIQUE (user_id, device_id);



--
-- Name: privchat_user_last_seen privchat_user_last_seen_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.privchat_user_last_seen
    ADD CONSTRAINT privchat_user_last_seen_pkey PRIMARY KEY (user_id);



--
-- Name: privchat_user_settings privchat_user_settings_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.privchat_user_settings
    ADD CONSTRAINT privchat_user_settings_pkey PRIMARY KEY (user_id, setting_key);



--
-- Name: privchat_users privchat_users_email_key; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.privchat_users
    ADD CONSTRAINT privchat_users_email_key UNIQUE (email);



--
-- Name: privchat_users privchat_users_phone_key; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.privchat_users
    ADD CONSTRAINT privchat_users_phone_key UNIQUE (phone);



--
-- Name: privchat_users privchat_users_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.privchat_users
    ADD CONSTRAINT privchat_users_pkey PRIMARY KEY (user_id);



--
-- Name: privchat_users privchat_users_username_key; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.privchat_users
    ADD CONSTRAINT privchat_users_username_key UNIQUE (username);



--
-- Name: privchat_group_pinned_messages uq_group_pinned_message; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.privchat_group_pinned_messages
    ADD CONSTRAINT uq_group_pinned_message UNIQUE (group_id, message_id);



--
-- Name: idx_bot_follow_bot; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_bot_follow_bot ON public.privchat_bot_follow USING btree (bot_user_id, status);



--
-- Name: idx_bot_follow_user; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_bot_follow_user ON public.privchat_bot_follow USING btree (user_id, status);



--
-- Name: idx_group_pinned_messages_group; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_group_pinned_messages_group ON public.privchat_group_pinned_messages USING btree (group_id, pinned_at DESC);



--
-- Name: idx_message_dedup_created_at; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_message_dedup_created_at ON public.privchat_message_dedup USING btree (created_at);



--
-- Name: idx_message_reactions_message_id; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_message_reactions_message_id ON public.privchat_message_reactions USING btree (message_id);



--
-- Name: idx_message_reactions_user_id; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_message_reactions_user_id ON public.privchat_message_reactions USING btree (user_id);



--
-- Name: idx_pgjr_group_status; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_pgjr_group_status ON public.privchat_group_join_requests USING btree (group_id, status);



--
-- Name: idx_pgjr_user; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_pgjr_user ON public.privchat_group_join_requests USING btree (user_id);



--
-- Name: idx_privchat_attachment_objects_path; Type: INDEX; Schema: public; Owner: -
--

CREATE UNIQUE INDEX idx_privchat_attachment_objects_path ON public.privchat_attachment_objects USING btree (storage_source_id, file_path);



--
-- Name: idx_privchat_blacklist_user; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_privchat_blacklist_user ON public.privchat_blacklist USING btree (user_id);



--
-- Name: idx_privchat_channel_participants_channel; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_privchat_channel_participants_channel ON public.privchat_channel_participants USING btree (channel_id) WHERE (left_at IS NULL);



--
-- Name: idx_privchat_channel_participants_role; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_privchat_channel_participants_role ON public.privchat_channel_participants USING btree (channel_id, role) WHERE (left_at IS NULL);



--
-- Name: idx_privchat_channel_participants_user; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_privchat_channel_participants_user ON public.privchat_channel_participants USING btree (user_id) WHERE (left_at IS NULL);



--
-- Name: idx_privchat_channel_pts_updated; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_privchat_channel_pts_updated ON public.privchat_channel_pts USING btree (updated_at);



--
-- Name: idx_privchat_channel_read_cursor_channel_sync_version; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_privchat_channel_read_cursor_channel_sync_version ON public.privchat_channel_read_cursor USING btree (channel_id, sync_version);



--
-- Name: idx_privchat_channel_read_cursor_channel_updated; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_privchat_channel_read_cursor_channel_updated ON public.privchat_channel_read_cursor USING btree (channel_id, updated_at DESC);



--
-- Name: idx_privchat_channel_read_cursor_user_sync_version; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_privchat_channel_read_cursor_user_sync_version ON public.privchat_channel_read_cursor USING btree (user_id, sync_version);



--
-- Name: idx_privchat_channel_read_cursor_user_updated; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_privchat_channel_read_cursor_user_updated ON public.privchat_channel_read_cursor USING btree (user_id, updated_at DESC);



--
-- Name: idx_privchat_channels_direct; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_privchat_channels_direct ON public.privchat_channels USING btree (direct_user1_id, direct_user2_id) WHERE (channel_type = 0);



--
-- Name: idx_privchat_channels_direct_unique; Type: INDEX; Schema: public; Owner: -
--

CREATE UNIQUE INDEX idx_privchat_channels_direct_unique ON public.privchat_channels USING btree (LEAST(direct_user1_id, direct_user2_id), GREATEST(direct_user1_id, direct_user2_id)) WHERE ((channel_type = 0) AND (direct_user1_id IS NOT NULL) AND (direct_user2_id IS NOT NULL));



--
-- Name: idx_privchat_channels_group; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_privchat_channels_group ON public.privchat_channels USING btree (group_id) WHERE (channel_type = 1);



--
-- Name: idx_privchat_channels_last_message; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_privchat_channels_last_message ON public.privchat_channels USING btree (last_message_at DESC);



--
-- Name: idx_privchat_channels_sync_version; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_privchat_channels_sync_version ON public.privchat_channels USING btree (sync_version);



--
-- Name: idx_privchat_channels_type; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_privchat_channels_type ON public.privchat_channels USING btree (channel_type);



--
-- Name: idx_privchat_client_msg_reg_created; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_privchat_client_msg_reg_created ON public.privchat_client_msg_registry USING btree (created_at);



--
-- Name: idx_privchat_client_msg_reg_server_id; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_privchat_client_msg_reg_server_id ON public.privchat_client_msg_registry USING btree (server_msg_id);



--
-- Name: idx_privchat_commit_log_channel_pts; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_privchat_commit_log_channel_pts ON public.privchat_commit_log USING btree (channel_id, pts);



--
-- Name: idx_privchat_commit_log_local_message_id; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_privchat_commit_log_local_message_id ON public.privchat_commit_log USING btree (local_message_id) WHERE (local_message_id IS NOT NULL);



--
-- Name: idx_privchat_commit_log_timestamp; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_privchat_commit_log_timestamp ON public.privchat_commit_log USING btree (server_timestamp);



--
-- Name: idx_privchat_delivery_receipts_sender; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_privchat_delivery_receipts_sender ON public.privchat_message_delivery_receipts USING btree (sender_id, delivered_at DESC);



--
-- Name: idx_privchat_devices_device; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_privchat_devices_device ON public.privchat_devices USING btree (device_id);



--
-- Name: idx_privchat_devices_kicked; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_privchat_devices_kicked ON public.privchat_devices USING btree (kicked_at DESC) WHERE (session_state = 1);



--
-- Name: idx_privchat_devices_session; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_privchat_devices_session ON public.privchat_devices USING btree (user_id, session_state);



--
-- Name: idx_privchat_devices_session_version; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_privchat_devices_session_version ON public.privchat_devices USING btree (user_id, session_version);



--
-- Name: idx_privchat_devices_user; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_privchat_devices_user ON public.privchat_devices USING btree (user_id);



--
-- Name: idx_privchat_devices_user_active; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_privchat_devices_user_active ON public.privchat_devices USING btree (user_id, last_active_at DESC);



--
-- Name: idx_privchat_dispatch_outbox_retention; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_privchat_dispatch_outbox_retention ON public.privchat_message_dispatch_outbox USING btree (status, created_at);



--
-- Name: idx_privchat_dispatch_recipient_claim; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_privchat_dispatch_recipient_claim ON public.privchat_message_dispatch_recipient USING btree (next_attempt_at, event_id, user_id) WHERE (state = 0);



--
-- Name: idx_privchat_file_uploads_business; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_privchat_file_uploads_business ON public.privchat_file_uploads USING btree (business_type, business_id);



--
-- Name: idx_privchat_file_uploads_object_id; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_privchat_file_uploads_object_id ON public.privchat_file_uploads USING btree (object_id);



--
-- Name: idx_privchat_file_uploads_uploaded_at; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_privchat_file_uploads_uploaded_at ON public.privchat_file_uploads USING btree (uploaded_at);



--
-- Name: idx_privchat_file_uploads_uploader_id; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_privchat_file_uploads_uploader_id ON public.privchat_file_uploads USING btree (uploader_id);



--
-- Name: idx_privchat_friendships_accepted; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_privchat_friendships_accepted ON public.privchat_friendships USING btree (user_id, friend_id) WHERE (status = 1);



--
-- Name: idx_privchat_friendships_friend; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_privchat_friendships_friend ON public.privchat_friendships USING btree (friend_id, status);



--
-- Name: idx_privchat_friendships_user; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_privchat_friendships_user ON public.privchat_friendships USING btree (user_id, status);



--
-- Name: idx_privchat_friendships_user_sync_version; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_privchat_friendships_user_sync_version ON public.privchat_friendships USING btree (user_id, sync_version DESC);



--
-- Name: idx_privchat_group_members_group; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_privchat_group_members_group ON public.privchat_group_members USING btree (group_id) WHERE (left_at IS NULL);



--
-- Name: idx_privchat_group_members_group_sync_version; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_privchat_group_members_group_sync_version ON public.privchat_group_members USING btree (group_id, sync_version DESC);



--
-- Name: idx_privchat_group_members_role; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_privchat_group_members_role ON public.privchat_group_members USING btree (group_id, role) WHERE (left_at IS NULL);



--
-- Name: idx_privchat_group_members_user; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_privchat_group_members_user ON public.privchat_group_members USING btree (user_id) WHERE (left_at IS NULL);



--
-- Name: idx_privchat_groups_owner; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_privchat_groups_owner ON public.privchat_groups USING btree (owner_id);



--
-- Name: idx_privchat_groups_settings_gin; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_privchat_groups_settings_gin ON public.privchat_groups USING gin (settings);



--
-- Name: idx_privchat_groups_status; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_privchat_groups_status ON public.privchat_groups USING btree (status) WHERE (status = 0);



--
-- Name: idx_privchat_groups_sync_version; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_privchat_groups_sync_version ON public.privchat_groups USING btree (sync_version DESC);



--
-- Name: idx_privchat_login_logs_created; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_privchat_login_logs_created ON public.privchat_login_logs USING btree (created_at DESC);



--
-- Name: idx_privchat_login_logs_device; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_privchat_login_logs_device ON public.privchat_login_logs USING btree (device_id, token_first_used_at DESC);



--
-- Name: idx_privchat_login_logs_ip; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_privchat_login_logs_ip ON public.privchat_login_logs USING btree (ip_address);



--
-- Name: idx_privchat_login_logs_new_device; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_privchat_login_logs_new_device ON public.privchat_login_logs USING btree (user_id, created_at DESC) WHERE (is_new_device = true);



--
-- Name: idx_privchat_login_logs_notification; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_privchat_login_logs_notification ON public.privchat_login_logs USING btree (user_id, notification_sent) WHERE (notification_sent = false);



--
-- Name: idx_privchat_login_logs_risk; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_privchat_login_logs_risk ON public.privchat_login_logs USING btree (user_id, risk_score DESC) WHERE (risk_score > 50);



--
-- Name: idx_privchat_login_logs_risk_factors; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_privchat_login_logs_risk_factors ON public.privchat_login_logs USING gin (risk_factors);



--
-- Name: idx_privchat_login_logs_status; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_privchat_login_logs_status ON public.privchat_login_logs USING btree (user_id, status);



--
-- Name: idx_privchat_login_logs_token_jti; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_privchat_login_logs_token_jti ON public.privchat_login_logs USING btree (token_jti);



--
-- Name: idx_privchat_login_logs_user_time; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_privchat_login_logs_user_time ON public.privchat_login_logs USING btree (user_id, token_first_used_at DESC);



--
-- Name: idx_privchat_message_file_refs_file; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_privchat_message_file_refs_file ON public.privchat_message_file_refs USING btree (file_id);



--
-- Name: idx_privchat_messages_channel_id; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_privchat_messages_channel_id ON ONLY public.privchat_messages USING btree (channel_id, message_id DESC);



--
-- Name: idx_privchat_messages_channel_pts; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_privchat_messages_channel_pts ON ONLY public.privchat_messages USING btree (channel_id, pts);



--
-- Name: idx_privchat_messages_channel_time; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_privchat_messages_channel_time ON ONLY public.privchat_messages USING btree (channel_id, created_at DESC);



--
-- Name: idx_privchat_messages_content_trgm; Type: INDEX; Schema: public; Owner: -
--

-- 🔴 pg_trgm 属于 contrib，托管 PostgreSQL 常常没装（Weey 生产库
-- pg_available_extensions 里只有 plpgsql）。无条件 CREATE EXTENSION 会让整条
-- 基线在这里断掉，而这条索引只是 admin 后台子串搜索的**性能优化**：客户端搜索
-- 走的是下面 privchat_search_tokens 的 bigram 索引，不依赖任何扩展。
-- 缺了就跳过，但要**明确告警**，不能装作没事发生。
DO $$
BEGIN
    IF EXISTS (SELECT 1 FROM pg_available_extensions WHERE name = 'pg_trgm') THEN
        CREATE EXTENSION IF NOT EXISTS pg_trgm WITH SCHEMA public;
        CREATE INDEX idx_privchat_messages_content_trgm
            ON ONLY public.privchat_messages USING gin (content public.gin_trgm_ops);
    ELSE
        RAISE WARNING '这台库没有 pg_trgm，跳过 admin 子串搜索的 GIN 索引：admin 消息搜索会退化为顺序扫描。客户端搜索不受影响。要恢复请在数据库主机安装 postgresql-contrib 后手工建此索引。';
    END IF;
END
$$;



--
-- Name: idx_privchat_messages_deleted; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_privchat_messages_deleted ON ONLY public.privchat_messages USING btree (channel_id, created_at DESC) WHERE (deleted = false);



--
-- Name: idx_privchat_messages_local_message_id; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_privchat_messages_local_message_id ON ONLY public.privchat_messages USING btree (channel_id, local_message_id) WHERE (local_message_id IS NOT NULL);



--
-- Name: idx_privchat_messages_metadata_gin; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_privchat_messages_metadata_gin ON ONLY public.privchat_messages USING gin (metadata);



--
-- Name: idx_privchat_messages_pts; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_privchat_messages_pts ON ONLY public.privchat_messages USING btree (sender_id, pts);



--
-- Name: idx_privchat_messages_reply; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_privchat_messages_reply ON ONLY public.privchat_messages USING btree (reply_to_message_id) WHERE (reply_to_message_id IS NOT NULL);



--
-- Name: idx_privchat_messages_revoked; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_privchat_messages_revoked ON ONLY public.privchat_messages USING btree (channel_id, revoked_at) WHERE (revoked = true);



--
-- Name: idx_privchat_messages_search_tokens; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_privchat_messages_search_tokens ON ONLY public.privchat_messages USING gin (public.privchat_search_tokens(content));



--
-- Name: idx_privchat_messages_sender; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_privchat_messages_sender ON ONLY public.privchat_messages USING btree (sender_id, created_at DESC);



--
-- Name: idx_privchat_messages_sender_time; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_privchat_messages_sender_time ON ONLY public.privchat_messages USING btree (sender_id, created_at DESC);



--
-- Name: idx_privchat_offline_queue_expires; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_privchat_offline_queue_expires ON public.privchat_offline_message_queue USING btree (expires_at);



--
-- Name: idx_privchat_offline_queue_user; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_privchat_offline_queue_user ON public.privchat_offline_message_queue USING btree (user_id, delivered, created_at);



--
-- Name: idx_privchat_read_receipts_channel; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_privchat_read_receipts_channel ON public.privchat_read_receipts USING btree (channel_id);



--
-- Name: idx_privchat_read_receipts_user_channel; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_privchat_read_receipts_user_channel ON public.privchat_read_receipts USING btree (user_id, channel_id);



--
-- Name: idx_privchat_refresh_tokens_expires_at; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_privchat_refresh_tokens_expires_at ON public.privchat_refresh_tokens USING btree (expires_at);



--
-- Name: idx_privchat_refresh_tokens_revoked_at; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_privchat_refresh_tokens_revoked_at ON public.privchat_refresh_tokens USING btree (revoked_at);



--
-- Name: idx_privchat_refresh_tokens_user_device; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_privchat_refresh_tokens_user_device ON public.privchat_refresh_tokens USING btree (user_id, device_id);



--
-- Name: idx_privchat_sync_state_channel; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_privchat_sync_state_channel ON public.privchat_device_sync_state USING btree (channel_id);



--
-- Name: idx_privchat_sync_state_user_device; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_privchat_sync_state_user_device ON public.privchat_device_sync_state USING btree (user_id, device_id);



--
-- Name: idx_privchat_user_channels_unread; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_privchat_user_channels_unread ON public.privchat_user_channels USING btree (user_id, unread_count) WHERE (unread_count > 0);



--
-- Name: idx_privchat_user_channels_user_pinned; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_privchat_user_channels_user_pinned ON public.privchat_user_channels USING btree (user_id, is_pinned DESC, updated_at DESC);



--
-- Name: idx_privchat_user_channels_user_sync_version; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_privchat_user_channels_user_sync_version ON public.privchat_user_channels USING btree (user_id, sync_version DESC);



--
-- Name: idx_privchat_user_channels_user_updated; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_privchat_user_channels_user_updated ON public.privchat_user_channels USING btree (user_id, updated_at DESC);



--
-- Name: idx_privchat_user_devices_apns_armed; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_privchat_user_devices_apns_armed ON public.privchat_user_devices USING btree (apns_armed) WHERE (apns_armed = true);



--
-- Name: idx_privchat_user_devices_connected; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_privchat_user_devices_connected ON public.privchat_user_devices USING btree (connected) WHERE (connected = true);



--
-- Name: idx_privchat_user_devices_user_id; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_privchat_user_devices_user_id ON public.privchat_user_devices USING btree (user_id);



--
-- Name: idx_privchat_user_last_seen_time; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_privchat_user_last_seen_time ON public.privchat_user_last_seen USING btree (last_seen_at);



--
-- Name: idx_privchat_user_settings_user_version; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_privchat_user_settings_user_version ON public.privchat_user_settings USING btree (user_id, version);



--
-- Name: idx_privchat_users_email; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_privchat_users_email ON public.privchat_users USING btree (email) WHERE (email IS NOT NULL);



--
-- Name: idx_privchat_users_last_active; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_privchat_users_last_active ON public.privchat_users USING btree (last_active_at DESC) WHERE (status = 0);



--
-- Name: idx_privchat_users_phone; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_privchat_users_phone ON public.privchat_users USING btree (phone) WHERE (phone IS NOT NULL);



--
-- Name: idx_privchat_users_status; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_privchat_users_status ON public.privchat_users USING btree (status) WHERE (status = 0);



--
-- Name: idx_privchat_users_sync_version; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_privchat_users_sync_version ON public.privchat_users USING btree (sync_version);



--
-- Name: idx_privchat_users_type; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_privchat_users_type ON public.privchat_users USING btree (user_type);



--
-- Name: idx_privchat_users_username; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_privchat_users_username ON public.privchat_users USING btree (username);



--
-- Name: privchat_messages_2026_01_channel_id_created_at_idx; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX privchat_messages_2026_01_channel_id_created_at_idx ON public.privchat_messages_2026_01 USING btree (channel_id, created_at DESC);



--
-- Name: privchat_messages_2026_01_channel_id_created_at_idx1; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX privchat_messages_2026_01_channel_id_created_at_idx1 ON public.privchat_messages_2026_01 USING btree (channel_id, created_at DESC) WHERE (deleted = false);



--
-- Name: privchat_messages_2026_01_channel_id_local_message_id_idx; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX privchat_messages_2026_01_channel_id_local_message_id_idx ON public.privchat_messages_2026_01 USING btree (channel_id, local_message_id) WHERE (local_message_id IS NOT NULL);



--
-- Name: privchat_messages_2026_01_channel_id_message_id_idx; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX privchat_messages_2026_01_channel_id_message_id_idx ON public.privchat_messages_2026_01 USING btree (channel_id, message_id DESC);



--
-- Name: privchat_messages_2026_01_channel_id_pts_idx; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX privchat_messages_2026_01_channel_id_pts_idx ON public.privchat_messages_2026_01 USING btree (channel_id, pts);



--
-- Name: privchat_messages_2026_01_channel_id_revoked_at_idx; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX privchat_messages_2026_01_channel_id_revoked_at_idx ON public.privchat_messages_2026_01 USING btree (channel_id, revoked_at) WHERE (revoked = true);



--
-- Name: privchat_messages_2026_01_content_idx; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX privchat_messages_2026_01_content_idx ON public.privchat_messages_2026_01 USING gin (content public.gin_trgm_ops);



--
-- Name: privchat_messages_2026_01_metadata_idx; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX privchat_messages_2026_01_metadata_idx ON public.privchat_messages_2026_01 USING gin (metadata);



--
-- Name: privchat_messages_2026_01_privchat_search_tokens_idx; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX privchat_messages_2026_01_privchat_search_tokens_idx ON public.privchat_messages_2026_01 USING gin (public.privchat_search_tokens(content));



--
-- Name: privchat_messages_2026_01_reply_to_message_id_idx; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX privchat_messages_2026_01_reply_to_message_id_idx ON public.privchat_messages_2026_01 USING btree (reply_to_message_id) WHERE (reply_to_message_id IS NOT NULL);



--
-- Name: privchat_messages_2026_01_sender_id_created_at_idx; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX privchat_messages_2026_01_sender_id_created_at_idx ON public.privchat_messages_2026_01 USING btree (sender_id, created_at DESC);



--
-- Name: privchat_messages_2026_01_sender_id_created_at_idx1; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX privchat_messages_2026_01_sender_id_created_at_idx1 ON public.privchat_messages_2026_01 USING btree (sender_id, created_at DESC);



--
-- Name: privchat_messages_2026_01_sender_id_pts_idx; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX privchat_messages_2026_01_sender_id_pts_idx ON public.privchat_messages_2026_01 USING btree (sender_id, pts);



--
-- Name: privchat_messages_2026_02_channel_id_created_at_idx; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX privchat_messages_2026_02_channel_id_created_at_idx ON public.privchat_messages_2026_02 USING btree (channel_id, created_at DESC);



--
-- Name: privchat_messages_2026_02_channel_id_created_at_idx1; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX privchat_messages_2026_02_channel_id_created_at_idx1 ON public.privchat_messages_2026_02 USING btree (channel_id, created_at DESC) WHERE (deleted = false);



--
-- Name: privchat_messages_2026_02_channel_id_local_message_id_idx; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX privchat_messages_2026_02_channel_id_local_message_id_idx ON public.privchat_messages_2026_02 USING btree (channel_id, local_message_id) WHERE (local_message_id IS NOT NULL);



--
-- Name: privchat_messages_2026_02_channel_id_message_id_idx; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX privchat_messages_2026_02_channel_id_message_id_idx ON public.privchat_messages_2026_02 USING btree (channel_id, message_id DESC);



--
-- Name: privchat_messages_2026_02_channel_id_pts_idx; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX privchat_messages_2026_02_channel_id_pts_idx ON public.privchat_messages_2026_02 USING btree (channel_id, pts);



--
-- Name: privchat_messages_2026_02_channel_id_revoked_at_idx; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX privchat_messages_2026_02_channel_id_revoked_at_idx ON public.privchat_messages_2026_02 USING btree (channel_id, revoked_at) WHERE (revoked = true);



--
-- Name: privchat_messages_2026_02_content_idx; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX privchat_messages_2026_02_content_idx ON public.privchat_messages_2026_02 USING gin (content public.gin_trgm_ops);



--
-- Name: privchat_messages_2026_02_metadata_idx; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX privchat_messages_2026_02_metadata_idx ON public.privchat_messages_2026_02 USING gin (metadata);



--
-- Name: privchat_messages_2026_02_privchat_search_tokens_idx; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX privchat_messages_2026_02_privchat_search_tokens_idx ON public.privchat_messages_2026_02 USING gin (public.privchat_search_tokens(content));



--
-- Name: privchat_messages_2026_02_reply_to_message_id_idx; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX privchat_messages_2026_02_reply_to_message_id_idx ON public.privchat_messages_2026_02 USING btree (reply_to_message_id) WHERE (reply_to_message_id IS NOT NULL);



--
-- Name: privchat_messages_2026_02_sender_id_created_at_idx; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX privchat_messages_2026_02_sender_id_created_at_idx ON public.privchat_messages_2026_02 USING btree (sender_id, created_at DESC);



--
-- Name: privchat_messages_2026_02_sender_id_created_at_idx1; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX privchat_messages_2026_02_sender_id_created_at_idx1 ON public.privchat_messages_2026_02 USING btree (sender_id, created_at DESC);



--
-- Name: privchat_messages_2026_02_sender_id_pts_idx; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX privchat_messages_2026_02_sender_id_pts_idx ON public.privchat_messages_2026_02 USING btree (sender_id, pts);



--
-- Name: privchat_messages_2026_03_channel_id_created_at_idx; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX privchat_messages_2026_03_channel_id_created_at_idx ON public.privchat_messages_2026_03 USING btree (channel_id, created_at DESC);



--
-- Name: privchat_messages_2026_03_channel_id_created_at_idx1; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX privchat_messages_2026_03_channel_id_created_at_idx1 ON public.privchat_messages_2026_03 USING btree (channel_id, created_at DESC) WHERE (deleted = false);



--
-- Name: privchat_messages_2026_03_channel_id_local_message_id_idx; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX privchat_messages_2026_03_channel_id_local_message_id_idx ON public.privchat_messages_2026_03 USING btree (channel_id, local_message_id) WHERE (local_message_id IS NOT NULL);



--
-- Name: privchat_messages_2026_03_channel_id_message_id_idx; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX privchat_messages_2026_03_channel_id_message_id_idx ON public.privchat_messages_2026_03 USING btree (channel_id, message_id DESC);



--
-- Name: privchat_messages_2026_03_channel_id_pts_idx; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX privchat_messages_2026_03_channel_id_pts_idx ON public.privchat_messages_2026_03 USING btree (channel_id, pts);



--
-- Name: privchat_messages_2026_03_channel_id_revoked_at_idx; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX privchat_messages_2026_03_channel_id_revoked_at_idx ON public.privchat_messages_2026_03 USING btree (channel_id, revoked_at) WHERE (revoked = true);



--
-- Name: privchat_messages_2026_03_content_idx; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX privchat_messages_2026_03_content_idx ON public.privchat_messages_2026_03 USING gin (content public.gin_trgm_ops);



--
-- Name: privchat_messages_2026_03_metadata_idx; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX privchat_messages_2026_03_metadata_idx ON public.privchat_messages_2026_03 USING gin (metadata);



--
-- Name: privchat_messages_2026_03_privchat_search_tokens_idx; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX privchat_messages_2026_03_privchat_search_tokens_idx ON public.privchat_messages_2026_03 USING gin (public.privchat_search_tokens(content));



--
-- Name: privchat_messages_2026_03_reply_to_message_id_idx; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX privchat_messages_2026_03_reply_to_message_id_idx ON public.privchat_messages_2026_03 USING btree (reply_to_message_id) WHERE (reply_to_message_id IS NOT NULL);



--
-- Name: privchat_messages_2026_03_sender_id_created_at_idx; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX privchat_messages_2026_03_sender_id_created_at_idx ON public.privchat_messages_2026_03 USING btree (sender_id, created_at DESC);



--
-- Name: privchat_messages_2026_03_sender_id_created_at_idx1; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX privchat_messages_2026_03_sender_id_created_at_idx1 ON public.privchat_messages_2026_03 USING btree (sender_id, created_at DESC);



--
-- Name: privchat_messages_2026_03_sender_id_pts_idx; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX privchat_messages_2026_03_sender_id_pts_idx ON public.privchat_messages_2026_03 USING btree (sender_id, pts);



--
-- Name: privchat_messages_2026_04_channel_id_created_at_idx; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX privchat_messages_2026_04_channel_id_created_at_idx ON public.privchat_messages_2026_04 USING btree (channel_id, created_at DESC);



--
-- Name: privchat_messages_2026_04_channel_id_created_at_idx1; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX privchat_messages_2026_04_channel_id_created_at_idx1 ON public.privchat_messages_2026_04 USING btree (channel_id, created_at DESC) WHERE (deleted = false);



--
-- Name: privchat_messages_2026_04_channel_id_local_message_id_idx; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX privchat_messages_2026_04_channel_id_local_message_id_idx ON public.privchat_messages_2026_04 USING btree (channel_id, local_message_id) WHERE (local_message_id IS NOT NULL);



--
-- Name: privchat_messages_2026_04_channel_id_message_id_idx; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX privchat_messages_2026_04_channel_id_message_id_idx ON public.privchat_messages_2026_04 USING btree (channel_id, message_id DESC);



--
-- Name: privchat_messages_2026_04_channel_id_pts_idx; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX privchat_messages_2026_04_channel_id_pts_idx ON public.privchat_messages_2026_04 USING btree (channel_id, pts);



--
-- Name: privchat_messages_2026_04_channel_id_revoked_at_idx; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX privchat_messages_2026_04_channel_id_revoked_at_idx ON public.privchat_messages_2026_04 USING btree (channel_id, revoked_at) WHERE (revoked = true);



--
-- Name: privchat_messages_2026_04_content_idx; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX privchat_messages_2026_04_content_idx ON public.privchat_messages_2026_04 USING gin (content public.gin_trgm_ops);



--
-- Name: privchat_messages_2026_04_metadata_idx; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX privchat_messages_2026_04_metadata_idx ON public.privchat_messages_2026_04 USING gin (metadata);



--
-- Name: privchat_messages_2026_04_privchat_search_tokens_idx; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX privchat_messages_2026_04_privchat_search_tokens_idx ON public.privchat_messages_2026_04 USING gin (public.privchat_search_tokens(content));



--
-- Name: privchat_messages_2026_04_reply_to_message_id_idx; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX privchat_messages_2026_04_reply_to_message_id_idx ON public.privchat_messages_2026_04 USING btree (reply_to_message_id) WHERE (reply_to_message_id IS NOT NULL);



--
-- Name: privchat_messages_2026_04_sender_id_created_at_idx; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX privchat_messages_2026_04_sender_id_created_at_idx ON public.privchat_messages_2026_04 USING btree (sender_id, created_at DESC);



--
-- Name: privchat_messages_2026_04_sender_id_created_at_idx1; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX privchat_messages_2026_04_sender_id_created_at_idx1 ON public.privchat_messages_2026_04 USING btree (sender_id, created_at DESC);



--
-- Name: privchat_messages_2026_04_sender_id_pts_idx; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX privchat_messages_2026_04_sender_id_pts_idx ON public.privchat_messages_2026_04 USING btree (sender_id, pts);



--
-- Name: privchat_messages_default_channel_id_created_at_idx; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX privchat_messages_default_channel_id_created_at_idx ON public.privchat_messages_default USING btree (channel_id, created_at DESC);



--
-- Name: privchat_messages_default_channel_id_created_at_idx1; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX privchat_messages_default_channel_id_created_at_idx1 ON public.privchat_messages_default USING btree (channel_id, created_at DESC) WHERE (deleted = false);



--
-- Name: privchat_messages_default_channel_id_local_message_id_idx; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX privchat_messages_default_channel_id_local_message_id_idx ON public.privchat_messages_default USING btree (channel_id, local_message_id) WHERE (local_message_id IS NOT NULL);



--
-- Name: privchat_messages_default_channel_id_message_id_idx; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX privchat_messages_default_channel_id_message_id_idx ON public.privchat_messages_default USING btree (channel_id, message_id DESC);



--
-- Name: privchat_messages_default_channel_id_pts_idx; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX privchat_messages_default_channel_id_pts_idx ON public.privchat_messages_default USING btree (channel_id, pts);



--
-- Name: privchat_messages_default_channel_id_revoked_at_idx; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX privchat_messages_default_channel_id_revoked_at_idx ON public.privchat_messages_default USING btree (channel_id, revoked_at) WHERE (revoked = true);



--
-- Name: privchat_messages_default_content_idx; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX privchat_messages_default_content_idx ON public.privchat_messages_default USING gin (content public.gin_trgm_ops);



--
-- Name: privchat_messages_default_metadata_idx; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX privchat_messages_default_metadata_idx ON public.privchat_messages_default USING gin (metadata);



--
-- Name: privchat_messages_default_privchat_search_tokens_idx; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX privchat_messages_default_privchat_search_tokens_idx ON public.privchat_messages_default USING gin (public.privchat_search_tokens(content));



--
-- Name: privchat_messages_default_reply_to_message_id_idx; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX privchat_messages_default_reply_to_message_id_idx ON public.privchat_messages_default USING btree (reply_to_message_id) WHERE (reply_to_message_id IS NOT NULL);



--
-- Name: privchat_messages_default_sender_id_created_at_idx; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX privchat_messages_default_sender_id_created_at_idx ON public.privchat_messages_default USING btree (sender_id, created_at DESC);



--
-- Name: privchat_messages_default_sender_id_created_at_idx1; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX privchat_messages_default_sender_id_created_at_idx1 ON public.privchat_messages_default USING btree (sender_id, created_at DESC);



--
-- Name: privchat_messages_default_sender_id_pts_idx; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX privchat_messages_default_sender_id_pts_idx ON public.privchat_messages_default USING btree (sender_id, pts);



--
-- Name: uk_bot_follow_user_bot; Type: INDEX; Schema: public; Owner: -
--

CREATE UNIQUE INDEX uk_bot_follow_user_bot ON public.privchat_bot_follow USING btree (user_id, bot_user_id);



--
-- Name: uq_pgjr_active_pending; Type: INDEX; Schema: public; Owner: -
--

CREATE UNIQUE INDEX uq_pgjr_active_pending ON public.privchat_group_join_requests USING btree (group_id, user_id) WHERE (status = 0);



--
-- Name: uq_privchat_file_uploads_claim_key; Type: INDEX; Schema: public; Owner: -
--

CREATE UNIQUE INDEX uq_privchat_file_uploads_claim_key ON public.privchat_file_uploads USING btree (uploader_id, claim_key_hash) WHERE (claim_key_hash IS NOT NULL);



--
-- Name: ux_privchat_groups_qr_key; Type: INDEX; Schema: public; Owner: -
--

CREATE UNIQUE INDEX ux_privchat_groups_qr_key ON public.privchat_groups USING btree (qr_key);



--
-- Name: ux_privchat_users_qr_key; Type: INDEX; Schema: public; Owner: -
--

CREATE UNIQUE INDEX ux_privchat_users_qr_key ON public.privchat_users USING btree (qr_key);



--
-- Name: privchat_messages_2026_01_channel_id_created_at_idx; Type: INDEX ATTACH; Schema: public; Owner: -
--

ALTER INDEX public.idx_privchat_messages_channel_time ATTACH PARTITION public.privchat_messages_2026_01_channel_id_created_at_idx;



--
-- Name: privchat_messages_2026_01_channel_id_created_at_idx1; Type: INDEX ATTACH; Schema: public; Owner: -
--

ALTER INDEX public.idx_privchat_messages_deleted ATTACH PARTITION public.privchat_messages_2026_01_channel_id_created_at_idx1;



--
-- Name: privchat_messages_2026_01_channel_id_local_message_id_idx; Type: INDEX ATTACH; Schema: public; Owner: -
--

ALTER INDEX public.idx_privchat_messages_local_message_id ATTACH PARTITION public.privchat_messages_2026_01_channel_id_local_message_id_idx;



--
-- Name: privchat_messages_2026_01_channel_id_message_id_idx; Type: INDEX ATTACH; Schema: public; Owner: -
--

ALTER INDEX public.idx_privchat_messages_channel_id ATTACH PARTITION public.privchat_messages_2026_01_channel_id_message_id_idx;



--
-- Name: privchat_messages_2026_01_channel_id_pts_idx; Type: INDEX ATTACH; Schema: public; Owner: -
--

ALTER INDEX public.idx_privchat_messages_channel_pts ATTACH PARTITION public.privchat_messages_2026_01_channel_id_pts_idx;



--
-- Name: privchat_messages_2026_01_channel_id_revoked_at_idx; Type: INDEX ATTACH; Schema: public; Owner: -
--

ALTER INDEX public.idx_privchat_messages_revoked ATTACH PARTITION public.privchat_messages_2026_01_channel_id_revoked_at_idx;



--
-- Name: privchat_messages_2026_01_content_idx; Type: INDEX ATTACH; Schema: public; Owner: -
--

DO $$
BEGIN
    IF to_regclass('public.idx_privchat_messages_content_trgm') IS NOT NULL THEN
        EXECUTE 'ALTER INDEX public.idx_privchat_messages_content_trgm ATTACH PARTITION public.privchat_messages_2026_01_content_idx';
    END IF;
END
$$;



--
-- Name: privchat_messages_2026_01_metadata_idx; Type: INDEX ATTACH; Schema: public; Owner: -
--

ALTER INDEX public.idx_privchat_messages_metadata_gin ATTACH PARTITION public.privchat_messages_2026_01_metadata_idx;



--
-- Name: privchat_messages_2026_01_pkey; Type: INDEX ATTACH; Schema: public; Owner: -
--

ALTER INDEX public.privchat_messages_pkey ATTACH PARTITION public.privchat_messages_2026_01_pkey;



--
-- Name: privchat_messages_2026_01_privchat_search_tokens_idx; Type: INDEX ATTACH; Schema: public; Owner: -
--

ALTER INDEX public.idx_privchat_messages_search_tokens ATTACH PARTITION public.privchat_messages_2026_01_privchat_search_tokens_idx;



--
-- Name: privchat_messages_2026_01_reply_to_message_id_idx; Type: INDEX ATTACH; Schema: public; Owner: -
--

ALTER INDEX public.idx_privchat_messages_reply ATTACH PARTITION public.privchat_messages_2026_01_reply_to_message_id_idx;



--
-- Name: privchat_messages_2026_01_sender_id_created_at_idx; Type: INDEX ATTACH; Schema: public; Owner: -
--

ALTER INDEX public.idx_privchat_messages_sender ATTACH PARTITION public.privchat_messages_2026_01_sender_id_created_at_idx;



--
-- Name: privchat_messages_2026_01_sender_id_created_at_idx1; Type: INDEX ATTACH; Schema: public; Owner: -
--

ALTER INDEX public.idx_privchat_messages_sender_time ATTACH PARTITION public.privchat_messages_2026_01_sender_id_created_at_idx1;



--
-- Name: privchat_messages_2026_01_sender_id_pts_idx; Type: INDEX ATTACH; Schema: public; Owner: -
--

ALTER INDEX public.idx_privchat_messages_pts ATTACH PARTITION public.privchat_messages_2026_01_sender_id_pts_idx;



--
-- Name: privchat_messages_2026_02_channel_id_created_at_idx; Type: INDEX ATTACH; Schema: public; Owner: -
--

ALTER INDEX public.idx_privchat_messages_channel_time ATTACH PARTITION public.privchat_messages_2026_02_channel_id_created_at_idx;



--
-- Name: privchat_messages_2026_02_channel_id_created_at_idx1; Type: INDEX ATTACH; Schema: public; Owner: -
--

ALTER INDEX public.idx_privchat_messages_deleted ATTACH PARTITION public.privchat_messages_2026_02_channel_id_created_at_idx1;



--
-- Name: privchat_messages_2026_02_channel_id_local_message_id_idx; Type: INDEX ATTACH; Schema: public; Owner: -
--

ALTER INDEX public.idx_privchat_messages_local_message_id ATTACH PARTITION public.privchat_messages_2026_02_channel_id_local_message_id_idx;



--
-- Name: privchat_messages_2026_02_channel_id_message_id_idx; Type: INDEX ATTACH; Schema: public; Owner: -
--

ALTER INDEX public.idx_privchat_messages_channel_id ATTACH PARTITION public.privchat_messages_2026_02_channel_id_message_id_idx;



--
-- Name: privchat_messages_2026_02_channel_id_pts_idx; Type: INDEX ATTACH; Schema: public; Owner: -
--

ALTER INDEX public.idx_privchat_messages_channel_pts ATTACH PARTITION public.privchat_messages_2026_02_channel_id_pts_idx;



--
-- Name: privchat_messages_2026_02_channel_id_revoked_at_idx; Type: INDEX ATTACH; Schema: public; Owner: -
--

ALTER INDEX public.idx_privchat_messages_revoked ATTACH PARTITION public.privchat_messages_2026_02_channel_id_revoked_at_idx;



--
-- Name: privchat_messages_2026_02_content_idx; Type: INDEX ATTACH; Schema: public; Owner: -
--

DO $$
BEGIN
    IF to_regclass('public.idx_privchat_messages_content_trgm') IS NOT NULL THEN
        EXECUTE 'ALTER INDEX public.idx_privchat_messages_content_trgm ATTACH PARTITION public.privchat_messages_2026_02_content_idx';
    END IF;
END
$$;



--
-- Name: privchat_messages_2026_02_metadata_idx; Type: INDEX ATTACH; Schema: public; Owner: -
--

ALTER INDEX public.idx_privchat_messages_metadata_gin ATTACH PARTITION public.privchat_messages_2026_02_metadata_idx;



--
-- Name: privchat_messages_2026_02_pkey; Type: INDEX ATTACH; Schema: public; Owner: -
--

ALTER INDEX public.privchat_messages_pkey ATTACH PARTITION public.privchat_messages_2026_02_pkey;



--
-- Name: privchat_messages_2026_02_privchat_search_tokens_idx; Type: INDEX ATTACH; Schema: public; Owner: -
--

ALTER INDEX public.idx_privchat_messages_search_tokens ATTACH PARTITION public.privchat_messages_2026_02_privchat_search_tokens_idx;



--
-- Name: privchat_messages_2026_02_reply_to_message_id_idx; Type: INDEX ATTACH; Schema: public; Owner: -
--

ALTER INDEX public.idx_privchat_messages_reply ATTACH PARTITION public.privchat_messages_2026_02_reply_to_message_id_idx;



--
-- Name: privchat_messages_2026_02_sender_id_created_at_idx; Type: INDEX ATTACH; Schema: public; Owner: -
--

ALTER INDEX public.idx_privchat_messages_sender ATTACH PARTITION public.privchat_messages_2026_02_sender_id_created_at_idx;



--
-- Name: privchat_messages_2026_02_sender_id_created_at_idx1; Type: INDEX ATTACH; Schema: public; Owner: -
--

ALTER INDEX public.idx_privchat_messages_sender_time ATTACH PARTITION public.privchat_messages_2026_02_sender_id_created_at_idx1;



--
-- Name: privchat_messages_2026_02_sender_id_pts_idx; Type: INDEX ATTACH; Schema: public; Owner: -
--

ALTER INDEX public.idx_privchat_messages_pts ATTACH PARTITION public.privchat_messages_2026_02_sender_id_pts_idx;



--
-- Name: privchat_messages_2026_03_channel_id_created_at_idx; Type: INDEX ATTACH; Schema: public; Owner: -
--

ALTER INDEX public.idx_privchat_messages_channel_time ATTACH PARTITION public.privchat_messages_2026_03_channel_id_created_at_idx;



--
-- Name: privchat_messages_2026_03_channel_id_created_at_idx1; Type: INDEX ATTACH; Schema: public; Owner: -
--

ALTER INDEX public.idx_privchat_messages_deleted ATTACH PARTITION public.privchat_messages_2026_03_channel_id_created_at_idx1;



--
-- Name: privchat_messages_2026_03_channel_id_local_message_id_idx; Type: INDEX ATTACH; Schema: public; Owner: -
--

ALTER INDEX public.idx_privchat_messages_local_message_id ATTACH PARTITION public.privchat_messages_2026_03_channel_id_local_message_id_idx;



--
-- Name: privchat_messages_2026_03_channel_id_message_id_idx; Type: INDEX ATTACH; Schema: public; Owner: -
--

ALTER INDEX public.idx_privchat_messages_channel_id ATTACH PARTITION public.privchat_messages_2026_03_channel_id_message_id_idx;



--
-- Name: privchat_messages_2026_03_channel_id_pts_idx; Type: INDEX ATTACH; Schema: public; Owner: -
--

ALTER INDEX public.idx_privchat_messages_channel_pts ATTACH PARTITION public.privchat_messages_2026_03_channel_id_pts_idx;



--
-- Name: privchat_messages_2026_03_channel_id_revoked_at_idx; Type: INDEX ATTACH; Schema: public; Owner: -
--

ALTER INDEX public.idx_privchat_messages_revoked ATTACH PARTITION public.privchat_messages_2026_03_channel_id_revoked_at_idx;



--
-- Name: privchat_messages_2026_03_content_idx; Type: INDEX ATTACH; Schema: public; Owner: -
--

DO $$
BEGIN
    IF to_regclass('public.idx_privchat_messages_content_trgm') IS NOT NULL THEN
        EXECUTE 'ALTER INDEX public.idx_privchat_messages_content_trgm ATTACH PARTITION public.privchat_messages_2026_03_content_idx';
    END IF;
END
$$;



--
-- Name: privchat_messages_2026_03_metadata_idx; Type: INDEX ATTACH; Schema: public; Owner: -
--

ALTER INDEX public.idx_privchat_messages_metadata_gin ATTACH PARTITION public.privchat_messages_2026_03_metadata_idx;



--
-- Name: privchat_messages_2026_03_pkey; Type: INDEX ATTACH; Schema: public; Owner: -
--

ALTER INDEX public.privchat_messages_pkey ATTACH PARTITION public.privchat_messages_2026_03_pkey;



--
-- Name: privchat_messages_2026_03_privchat_search_tokens_idx; Type: INDEX ATTACH; Schema: public; Owner: -
--

ALTER INDEX public.idx_privchat_messages_search_tokens ATTACH PARTITION public.privchat_messages_2026_03_privchat_search_tokens_idx;



--
-- Name: privchat_messages_2026_03_reply_to_message_id_idx; Type: INDEX ATTACH; Schema: public; Owner: -
--

ALTER INDEX public.idx_privchat_messages_reply ATTACH PARTITION public.privchat_messages_2026_03_reply_to_message_id_idx;



--
-- Name: privchat_messages_2026_03_sender_id_created_at_idx; Type: INDEX ATTACH; Schema: public; Owner: -
--

ALTER INDEX public.idx_privchat_messages_sender ATTACH PARTITION public.privchat_messages_2026_03_sender_id_created_at_idx;



--
-- Name: privchat_messages_2026_03_sender_id_created_at_idx1; Type: INDEX ATTACH; Schema: public; Owner: -
--

ALTER INDEX public.idx_privchat_messages_sender_time ATTACH PARTITION public.privchat_messages_2026_03_sender_id_created_at_idx1;



--
-- Name: privchat_messages_2026_03_sender_id_pts_idx; Type: INDEX ATTACH; Schema: public; Owner: -
--

ALTER INDEX public.idx_privchat_messages_pts ATTACH PARTITION public.privchat_messages_2026_03_sender_id_pts_idx;



--
-- Name: privchat_messages_2026_04_channel_id_created_at_idx; Type: INDEX ATTACH; Schema: public; Owner: -
--

ALTER INDEX public.idx_privchat_messages_channel_time ATTACH PARTITION public.privchat_messages_2026_04_channel_id_created_at_idx;



--
-- Name: privchat_messages_2026_04_channel_id_created_at_idx1; Type: INDEX ATTACH; Schema: public; Owner: -
--

ALTER INDEX public.idx_privchat_messages_deleted ATTACH PARTITION public.privchat_messages_2026_04_channel_id_created_at_idx1;



--
-- Name: privchat_messages_2026_04_channel_id_local_message_id_idx; Type: INDEX ATTACH; Schema: public; Owner: -
--

ALTER INDEX public.idx_privchat_messages_local_message_id ATTACH PARTITION public.privchat_messages_2026_04_channel_id_local_message_id_idx;



--
-- Name: privchat_messages_2026_04_channel_id_message_id_idx; Type: INDEX ATTACH; Schema: public; Owner: -
--

ALTER INDEX public.idx_privchat_messages_channel_id ATTACH PARTITION public.privchat_messages_2026_04_channel_id_message_id_idx;



--
-- Name: privchat_messages_2026_04_channel_id_pts_idx; Type: INDEX ATTACH; Schema: public; Owner: -
--

ALTER INDEX public.idx_privchat_messages_channel_pts ATTACH PARTITION public.privchat_messages_2026_04_channel_id_pts_idx;



--
-- Name: privchat_messages_2026_04_channel_id_revoked_at_idx; Type: INDEX ATTACH; Schema: public; Owner: -
--

ALTER INDEX public.idx_privchat_messages_revoked ATTACH PARTITION public.privchat_messages_2026_04_channel_id_revoked_at_idx;



--
-- Name: privchat_messages_2026_04_content_idx; Type: INDEX ATTACH; Schema: public; Owner: -
--

DO $$
BEGIN
    IF to_regclass('public.idx_privchat_messages_content_trgm') IS NOT NULL THEN
        EXECUTE 'ALTER INDEX public.idx_privchat_messages_content_trgm ATTACH PARTITION public.privchat_messages_2026_04_content_idx';
    END IF;
END
$$;



--
-- Name: privchat_messages_2026_04_metadata_idx; Type: INDEX ATTACH; Schema: public; Owner: -
--

ALTER INDEX public.idx_privchat_messages_metadata_gin ATTACH PARTITION public.privchat_messages_2026_04_metadata_idx;



--
-- Name: privchat_messages_2026_04_pkey; Type: INDEX ATTACH; Schema: public; Owner: -
--

ALTER INDEX public.privchat_messages_pkey ATTACH PARTITION public.privchat_messages_2026_04_pkey;



--
-- Name: privchat_messages_2026_04_privchat_search_tokens_idx; Type: INDEX ATTACH; Schema: public; Owner: -
--

ALTER INDEX public.idx_privchat_messages_search_tokens ATTACH PARTITION public.privchat_messages_2026_04_privchat_search_tokens_idx;



--
-- Name: privchat_messages_2026_04_reply_to_message_id_idx; Type: INDEX ATTACH; Schema: public; Owner: -
--

ALTER INDEX public.idx_privchat_messages_reply ATTACH PARTITION public.privchat_messages_2026_04_reply_to_message_id_idx;



--
-- Name: privchat_messages_2026_04_sender_id_created_at_idx; Type: INDEX ATTACH; Schema: public; Owner: -
--

ALTER INDEX public.idx_privchat_messages_sender ATTACH PARTITION public.privchat_messages_2026_04_sender_id_created_at_idx;



--
-- Name: privchat_messages_2026_04_sender_id_created_at_idx1; Type: INDEX ATTACH; Schema: public; Owner: -
--

ALTER INDEX public.idx_privchat_messages_sender_time ATTACH PARTITION public.privchat_messages_2026_04_sender_id_created_at_idx1;



--
-- Name: privchat_messages_2026_04_sender_id_pts_idx; Type: INDEX ATTACH; Schema: public; Owner: -
--

ALTER INDEX public.idx_privchat_messages_pts ATTACH PARTITION public.privchat_messages_2026_04_sender_id_pts_idx;



--
-- Name: privchat_messages_default_channel_id_created_at_idx; Type: INDEX ATTACH; Schema: public; Owner: -
--

ALTER INDEX public.idx_privchat_messages_channel_time ATTACH PARTITION public.privchat_messages_default_channel_id_created_at_idx;



--
-- Name: privchat_messages_default_channel_id_created_at_idx1; Type: INDEX ATTACH; Schema: public; Owner: -
--

ALTER INDEX public.idx_privchat_messages_deleted ATTACH PARTITION public.privchat_messages_default_channel_id_created_at_idx1;



--
-- Name: privchat_messages_default_channel_id_local_message_id_idx; Type: INDEX ATTACH; Schema: public; Owner: -
--

ALTER INDEX public.idx_privchat_messages_local_message_id ATTACH PARTITION public.privchat_messages_default_channel_id_local_message_id_idx;



--
-- Name: privchat_messages_default_channel_id_message_id_idx; Type: INDEX ATTACH; Schema: public; Owner: -
--

ALTER INDEX public.idx_privchat_messages_channel_id ATTACH PARTITION public.privchat_messages_default_channel_id_message_id_idx;



--
-- Name: privchat_messages_default_channel_id_pts_idx; Type: INDEX ATTACH; Schema: public; Owner: -
--

ALTER INDEX public.idx_privchat_messages_channel_pts ATTACH PARTITION public.privchat_messages_default_channel_id_pts_idx;



--
-- Name: privchat_messages_default_channel_id_revoked_at_idx; Type: INDEX ATTACH; Schema: public; Owner: -
--

ALTER INDEX public.idx_privchat_messages_revoked ATTACH PARTITION public.privchat_messages_default_channel_id_revoked_at_idx;



--
-- Name: privchat_messages_default_content_idx; Type: INDEX ATTACH; Schema: public; Owner: -
--

DO $$
BEGIN
    IF to_regclass('public.idx_privchat_messages_content_trgm') IS NOT NULL THEN
        EXECUTE 'ALTER INDEX public.idx_privchat_messages_content_trgm ATTACH PARTITION public.privchat_messages_default_content_idx';
    END IF;
END
$$;



--
-- Name: privchat_messages_default_metadata_idx; Type: INDEX ATTACH; Schema: public; Owner: -
--

ALTER INDEX public.idx_privchat_messages_metadata_gin ATTACH PARTITION public.privchat_messages_default_metadata_idx;



--
-- Name: privchat_messages_default_pkey; Type: INDEX ATTACH; Schema: public; Owner: -
--

ALTER INDEX public.privchat_messages_pkey ATTACH PARTITION public.privchat_messages_default_pkey;



--
-- Name: privchat_messages_default_privchat_search_tokens_idx; Type: INDEX ATTACH; Schema: public; Owner: -
--

ALTER INDEX public.idx_privchat_messages_search_tokens ATTACH PARTITION public.privchat_messages_default_privchat_search_tokens_idx;



--
-- Name: privchat_messages_default_reply_to_message_id_idx; Type: INDEX ATTACH; Schema: public; Owner: -
--

ALTER INDEX public.idx_privchat_messages_reply ATTACH PARTITION public.privchat_messages_default_reply_to_message_id_idx;



--
-- Name: privchat_messages_default_sender_id_created_at_idx; Type: INDEX ATTACH; Schema: public; Owner: -
--

ALTER INDEX public.idx_privchat_messages_sender ATTACH PARTITION public.privchat_messages_default_sender_id_created_at_idx;



--
-- Name: privchat_messages_default_sender_id_created_at_idx1; Type: INDEX ATTACH; Schema: public; Owner: -
--

ALTER INDEX public.idx_privchat_messages_sender_time ATTACH PARTITION public.privchat_messages_default_sender_id_created_at_idx1;



--
-- Name: privchat_messages_default_sender_id_pts_idx; Type: INDEX ATTACH; Schema: public; Owner: -
--

ALTER INDEX public.idx_privchat_messages_pts ATTACH PARTITION public.privchat_messages_default_sender_id_pts_idx;



--
-- Name: privchat_channels privchat_channels_sync_version_trigger; Type: TRIGGER; Schema: public; Owner: -
--

CREATE TRIGGER privchat_channels_sync_version_trigger BEFORE UPDATE ON public.privchat_channels FOR EACH ROW EXECUTE FUNCTION public.assign_privchat_channel_entity_sync_version();



--
-- Name: privchat_friendships privchat_friendships_sync_version_trigger; Type: TRIGGER; Schema: public; Owner: -
--

CREATE TRIGGER privchat_friendships_sync_version_trigger BEFORE UPDATE ON public.privchat_friendships FOR EACH ROW EXECUTE FUNCTION public.assign_privchat_friend_sync_version();



--
-- Name: privchat_group_members privchat_group_members_sync_version_trigger; Type: TRIGGER; Schema: public; Owner: -
--

CREATE TRIGGER privchat_group_members_sync_version_trigger BEFORE UPDATE ON public.privchat_group_members FOR EACH ROW EXECUTE FUNCTION public.assign_privchat_group_member_sync_version();



--
-- Name: privchat_groups privchat_groups_sync_version_trigger; Type: TRIGGER; Schema: public; Owner: -
--

CREATE TRIGGER privchat_groups_sync_version_trigger BEFORE UPDATE ON public.privchat_groups FOR EACH ROW EXECUTE FUNCTION public.assign_privchat_group_sync_version();



--
-- Name: privchat_user_channels privchat_user_channels_sync_version_trigger; Type: TRIGGER; Schema: public; Owner: -
--

CREATE TRIGGER privchat_user_channels_sync_version_trigger BEFORE UPDATE ON public.privchat_user_channels FOR EACH ROW EXECUTE FUNCTION public.assign_privchat_channel_entity_sync_version();



--
-- Name: privchat_users privchat_users_sync_version_trigger; Type: TRIGGER; Schema: public; Owner: -
--

CREATE TRIGGER privchat_users_sync_version_trigger BEFORE UPDATE ON public.privchat_users FOR EACH ROW EXECUTE FUNCTION public.assign_privchat_user_sync_version();



--
-- Name: privchat_channel_read_cursor trg_privchat_channel_read_cursor_sync_version; Type: TRIGGER; Schema: public; Owner: -
--

CREATE TRIGGER trg_privchat_channel_read_cursor_sync_version BEFORE INSERT OR UPDATE ON public.privchat_channel_read_cursor FOR EACH ROW EXECUTE FUNCTION public.privchat_set_channel_read_cursor_sync_version();



--
-- Name: privchat_group_members trg_privchat_group_membership_version; Type: TRIGGER; Schema: public; Owner: -
--

CREATE TRIGGER trg_privchat_group_membership_version AFTER INSERT OR DELETE OR UPDATE OF left_at ON public.privchat_group_members FOR EACH ROW EXECUTE FUNCTION public.privchat_bump_group_membership_version();



--
-- Name: privchat_channel_pts update_privchat_channel_pts_updated_at; Type: TRIGGER; Schema: public; Owner: -
--

CREATE TRIGGER update_privchat_channel_pts_updated_at BEFORE UPDATE ON public.privchat_channel_pts FOR EACH ROW EXECUTE FUNCTION public.update_updated_at_column();



--
-- Name: privchat_devices update_privchat_devices_updated_at; Type: TRIGGER; Schema: public; Owner: -
--

CREATE TRIGGER update_privchat_devices_updated_at BEFORE UPDATE ON public.privchat_devices FOR EACH ROW EXECUTE FUNCTION public.update_updated_at_column();



--
-- Name: privchat_group_members update_privchat_group_members_updated_at; Type: TRIGGER; Schema: public; Owner: -
--

CREATE TRIGGER update_privchat_group_members_updated_at BEFORE UPDATE ON public.privchat_group_members FOR EACH ROW EXECUTE FUNCTION public.update_updated_at_column();



--
-- Name: privchat_blacklist privchat_blacklist_blocked_user_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.privchat_blacklist
    ADD CONSTRAINT privchat_blacklist_blocked_user_id_fkey FOREIGN KEY (blocked_user_id) REFERENCES public.privchat_users(user_id) ON DELETE CASCADE;



--
-- Name: privchat_blacklist privchat_blacklist_user_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.privchat_blacklist
    ADD CONSTRAINT privchat_blacklist_user_id_fkey FOREIGN KEY (user_id) REFERENCES public.privchat_users(user_id) ON DELETE CASCADE;



--
-- Name: privchat_channel_participants privchat_channel_participants_channel_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.privchat_channel_participants
    ADD CONSTRAINT privchat_channel_participants_channel_id_fkey FOREIGN KEY (channel_id) REFERENCES public.privchat_channels(channel_id) ON DELETE CASCADE;



--
-- Name: privchat_channel_participants privchat_channel_participants_user_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.privchat_channel_participants
    ADD CONSTRAINT privchat_channel_participants_user_id_fkey FOREIGN KEY (user_id) REFERENCES public.privchat_users(user_id) ON DELETE CASCADE;



--
-- Name: privchat_channels privchat_channels_direct_user1_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.privchat_channels
    ADD CONSTRAINT privchat_channels_direct_user1_id_fkey FOREIGN KEY (direct_user1_id) REFERENCES public.privchat_users(user_id);



--
-- Name: privchat_channels privchat_channels_direct_user2_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.privchat_channels
    ADD CONSTRAINT privchat_channels_direct_user2_id_fkey FOREIGN KEY (direct_user2_id) REFERENCES public.privchat_users(user_id);



--
-- Name: privchat_channels privchat_channels_group_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.privchat_channels
    ADD CONSTRAINT privchat_channels_group_id_fkey FOREIGN KEY (group_id) REFERENCES public.privchat_groups(group_id);



--
-- Name: privchat_client_msg_registry privchat_client_msg_registry_sender_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.privchat_client_msg_registry
    ADD CONSTRAINT privchat_client_msg_registry_sender_id_fkey FOREIGN KEY (sender_id) REFERENCES public.privchat_users(user_id);



--
-- Name: privchat_commit_log privchat_commit_log_sender_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.privchat_commit_log
    ADD CONSTRAINT privchat_commit_log_sender_id_fkey FOREIGN KEY (sender_id) REFERENCES public.privchat_users(user_id);



--
-- Name: privchat_device_sync_state privchat_device_sync_state_channel_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.privchat_device_sync_state
    ADD CONSTRAINT privchat_device_sync_state_channel_id_fkey FOREIGN KEY (channel_id) REFERENCES public.privchat_channels(channel_id) ON DELETE CASCADE;



--
-- Name: privchat_device_sync_state privchat_device_sync_state_user_id_device_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.privchat_device_sync_state
    ADD CONSTRAINT privchat_device_sync_state_user_id_device_id_fkey FOREIGN KEY (user_id, device_id) REFERENCES public.privchat_devices(user_id, device_id) ON DELETE CASCADE;



--
-- Name: privchat_device_sync_state privchat_device_sync_state_user_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.privchat_device_sync_state
    ADD CONSTRAINT privchat_device_sync_state_user_id_fkey FOREIGN KEY (user_id) REFERENCES public.privchat_users(user_id) ON DELETE CASCADE;



--
-- Name: privchat_devices privchat_devices_user_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.privchat_devices
    ADD CONSTRAINT privchat_devices_user_id_fkey FOREIGN KEY (user_id) REFERENCES public.privchat_users(user_id) ON DELETE CASCADE;



--
-- Name: privchat_file_uploads privchat_file_uploads_object_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.privchat_file_uploads
    ADD CONSTRAINT privchat_file_uploads_object_id_fkey FOREIGN KEY (object_id) REFERENCES public.privchat_attachment_objects(object_id) ON DELETE RESTRICT;



--
-- Name: privchat_friendships privchat_friendships_friend_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.privchat_friendships
    ADD CONSTRAINT privchat_friendships_friend_id_fkey FOREIGN KEY (friend_id) REFERENCES public.privchat_users(user_id) ON DELETE CASCADE;



--
-- Name: privchat_friendships privchat_friendships_user_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.privchat_friendships
    ADD CONSTRAINT privchat_friendships_user_id_fkey FOREIGN KEY (user_id) REFERENCES public.privchat_users(user_id) ON DELETE CASCADE;



--
-- Name: privchat_group_members privchat_group_members_group_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.privchat_group_members
    ADD CONSTRAINT privchat_group_members_group_id_fkey FOREIGN KEY (group_id) REFERENCES public.privchat_groups(group_id) ON DELETE CASCADE;



--
-- Name: privchat_group_members privchat_group_members_user_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.privchat_group_members
    ADD CONSTRAINT privchat_group_members_user_id_fkey FOREIGN KEY (user_id) REFERENCES public.privchat_users(user_id) ON DELETE CASCADE;



--
-- Name: privchat_groups privchat_groups_owner_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.privchat_groups
    ADD CONSTRAINT privchat_groups_owner_id_fkey FOREIGN KEY (owner_id) REFERENCES public.privchat_users(user_id);



--
-- Name: privchat_login_logs privchat_login_logs_user_id_device_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.privchat_login_logs
    ADD CONSTRAINT privchat_login_logs_user_id_device_id_fkey FOREIGN KEY (user_id, device_id) REFERENCES public.privchat_devices(user_id, device_id) ON DELETE CASCADE;



--
-- Name: privchat_login_logs privchat_login_logs_user_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.privchat_login_logs
    ADD CONSTRAINT privchat_login_logs_user_id_fkey FOREIGN KEY (user_id) REFERENCES public.privchat_users(user_id) ON DELETE CASCADE;



--
-- Name: privchat_message_dispatch_outbox privchat_message_dispatch_outbox_event_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.privchat_message_dispatch_outbox
    ADD CONSTRAINT privchat_message_dispatch_outbox_event_id_fkey FOREIGN KEY (event_id) REFERENCES public.privchat_commit_log(id) ON DELETE CASCADE;



--
-- Name: privchat_message_dispatch_recipient privchat_message_dispatch_recipient_event_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.privchat_message_dispatch_recipient
    ADD CONSTRAINT privchat_message_dispatch_recipient_event_id_fkey FOREIGN KEY (event_id) REFERENCES public.privchat_message_dispatch_outbox(event_id) ON DELETE CASCADE;



--
-- Name: privchat_message_file_refs privchat_message_file_refs_message_id_message_created_at_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.privchat_message_file_refs
    ADD CONSTRAINT privchat_message_file_refs_message_id_message_created_at_fkey FOREIGN KEY (message_id, message_created_at) REFERENCES public.privchat_messages(message_id, created_at) ON DELETE CASCADE;



--
-- Name: privchat_message_reactions privchat_message_reactions_user_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.privchat_message_reactions
    ADD CONSTRAINT privchat_message_reactions_user_id_fkey FOREIGN KEY (user_id) REFERENCES public.privchat_users(user_id) ON DELETE CASCADE;



--
-- Name: privchat_messages privchat_messages_channel_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE public.privchat_messages
    ADD CONSTRAINT privchat_messages_channel_id_fkey FOREIGN KEY (channel_id) REFERENCES public.privchat_channels(channel_id) ON DELETE CASCADE;



--
-- Name: privchat_messages privchat_messages_revoked_by_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE public.privchat_messages
    ADD CONSTRAINT privchat_messages_revoked_by_fkey FOREIGN KEY (revoked_by) REFERENCES public.privchat_users(user_id);



--
-- Name: privchat_messages privchat_messages_sender_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE public.privchat_messages
    ADD CONSTRAINT privchat_messages_sender_id_fkey FOREIGN KEY (sender_id) REFERENCES public.privchat_users(user_id);



--
-- Name: privchat_offline_message_queue privchat_offline_message_queue_user_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.privchat_offline_message_queue
    ADD CONSTRAINT privchat_offline_message_queue_user_id_fkey FOREIGN KEY (user_id) REFERENCES public.privchat_users(user_id) ON DELETE CASCADE;



--
-- Name: privchat_read_receipts privchat_read_receipts_channel_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.privchat_read_receipts
    ADD CONSTRAINT privchat_read_receipts_channel_id_fkey FOREIGN KEY (channel_id) REFERENCES public.privchat_channels(channel_id) ON DELETE CASCADE;



--
-- Name: privchat_read_receipts privchat_read_receipts_user_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.privchat_read_receipts
    ADD CONSTRAINT privchat_read_receipts_user_id_fkey FOREIGN KEY (user_id) REFERENCES public.privchat_users(user_id) ON DELETE CASCADE;



--
-- Name: privchat_user_channels privchat_user_channels_channel_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.privchat_user_channels
    ADD CONSTRAINT privchat_user_channels_channel_id_fkey FOREIGN KEY (channel_id) REFERENCES public.privchat_channels(channel_id) ON DELETE CASCADE;



--
-- Name: privchat_user_channels privchat_user_channels_user_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.privchat_user_channels
    ADD CONSTRAINT privchat_user_channels_user_id_fkey FOREIGN KEY (user_id) REFERENCES public.privchat_users(user_id) ON DELETE CASCADE;



--
-- Name: privchat_user_last_seen privchat_user_last_seen_user_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.privchat_user_last_seen
    ADD CONSTRAINT privchat_user_last_seen_user_id_fkey FOREIGN KEY (user_id) REFERENCES public.privchat_users(user_id) ON DELETE CASCADE;



--
-- Name: privchat_user_settings privchat_user_settings_user_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.privchat_user_settings
    ADD CONSTRAINT privchat_user_settings_user_id_fkey FOREIGN KEY (user_id) REFERENCES public.privchat_users(user_id) ON DELETE CASCADE;


--
-- PostgreSQL database dump complete
--

