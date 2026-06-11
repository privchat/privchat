-- ==============================================================================
-- PrivChat Server 初始化 SQL
-- 由 pg_dump --schema-only 从 dev 库 (已应用过原 001..018) 导出, 重整为干净的
-- single-file initial schema. 后续 schema 变更走新的 002+ 增量迁移。
-- ==============================================================================

--
-- PostgreSQL database dump
--

-- Dumped from database version 16.9 (Homebrew)
-- Dumped by pg_dump version 16.9 (Homebrew)

SET statement_timeout = 0;
SET lock_timeout = 0;
SET idle_in_transaction_session_timeout = 0;
SET client_encoding = 'UTF8';
SET standard_conforming_strings = on;
SELECT pg_catalog.set_config('search_path', '', false);
SET check_function_bodies = false;
SET xmloption = content;
SET client_min_messages = warning;
SET row_security = off;

-- pgcrypto extension 由 DBA 用 postgres 超管预先装好 (CREATE EXTENSION pgcrypto), 本文件不再 create.
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
-- Name: privchat_blacklist; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.privchat_blacklist (
    user_id bigint NOT NULL,
    blocked_user_id bigint NOT NULL,
    created_at bigint DEFAULT public.now_millis() NOT NULL
);


--
-- Name: TABLE privchat_blacklist; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON TABLE public.privchat_blacklist IS '黑名单表：存储用户黑名单';


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
-- Name: TABLE privchat_channel_participants; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON TABLE public.privchat_channel_participants IS '频道参与者表：存储频道参与者信息';


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
-- Name: TABLE privchat_channel_pts; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON TABLE public.privchat_channel_pts IS '频道 pts 表：存储每个频道的当前 pts（per-channel 单调递增）';


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
    CONSTRAINT privchat_channels_check CHECK ((((channel_type = 0) AND (direct_user1_id IS NOT NULL) AND (direct_user2_id IS NOT NULL) AND (group_id IS NULL)) OR ((channel_type = 1) AND (group_id IS NOT NULL) AND (direct_user1_id IS NULL) AND (direct_user2_id IS NULL))))
);


--
-- Name: TABLE privchat_channels; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON TABLE public.privchat_channels IS '频道表：存储频道基本信息（channel_id: BIGSERIAL）';


--
-- Name: COLUMN privchat_channels.create_source; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public.privchat_channels.create_source IS '创建来源类型: search/phone/card_share/group/qrcode 等';


--
-- Name: COLUMN privchat_channels.create_source_id; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public.privchat_channels.create_source_id IS '来源ID: 搜索会话id、群id、分享id、好友id 等';


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
    created_at bigint DEFAULT public.now_millis() NOT NULL
);


--
-- Name: TABLE privchat_client_msg_registry; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON TABLE public.privchat_client_msg_registry IS '客户端消息号注册表：用于幂等性检查';


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
    created_at bigint DEFAULT public.now_millis() NOT NULL
);


--
-- Name: TABLE privchat_commit_log; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON TABLE public.privchat_commit_log IS 'Commit Log：权威事实，按 pts 严格递增';


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
-- Name: TABLE privchat_device_sync_state; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON TABLE public.privchat_device_sync_state IS '设备同步状态表：存储设备的 pts 同步状态';


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
-- Name: TABLE privchat_devices; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON TABLE public.privchat_devices IS '设备表：存储用户设备信息（device_id: UUID）';


--
-- Name: COLUMN privchat_devices.session_version; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public.privchat_devices.session_version IS '会话世代，只在安全事件时递增（登出、改密、被踢等）';


--
-- Name: COLUMN privchat_devices.session_state; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public.privchat_devices.session_state IS '会话状态：0=活跃,1=被踢,2=冻结,3=撤销,4=待验证';


--
-- Name: COLUMN privchat_devices.kicked_at; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public.privchat_devices.kicked_at IS '被踢时间（毫秒时间戳）';


--
-- Name: COLUMN privchat_devices.kicked_by_device_id; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public.privchat_devices.kicked_by_device_id IS '由哪个设备踢出';


--
-- Name: COLUMN privchat_devices.kicked_reason; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public.privchat_devices.kicked_reason IS '踢出原因或状态变更原因';


--
-- Name: COLUMN privchat_devices.last_ip; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public.privchat_devices.last_ip IS '最后活跃IP地址';


--
-- Name: privchat_file_uploads; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.privchat_file_uploads (
    file_id bigint NOT NULL,
    original_filename character varying(512) NOT NULL,
    file_size bigint NOT NULL,
    file_type character varying(32) NOT NULL,
    mime_type character varying(128) NOT NULL,
    file_path text NOT NULL,
    storage_source_id integer DEFAULT 0 NOT NULL,
    uploader_id bigint NOT NULL,
    uploader_ip character varying(45),
    uploaded_at bigint DEFAULT public.now_millis() NOT NULL,
    width integer,
    height integer,
    file_hash character varying(128),
    business_type character varying(64),
    business_id character varying(128)
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
-- Name: TABLE privchat_friendships; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON TABLE public.privchat_friendships IS '好友关系表：存储用户之间的好友关系';


--
-- Name: COLUMN privchat_friendships.source_id; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public.privchat_friendships.source_id IS '来源ID，与 source 配套，可追溯';


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
-- Name: TABLE privchat_group_members; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON TABLE public.privchat_group_members IS '群组成员表：存储群组成员信息';


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
    group_id bigint NOT NULL,
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
    qr_key character varying(16) NOT NULL
);


--
-- Name: TABLE privchat_groups; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON TABLE public.privchat_groups IS '群组表：存储群组基本信息（group_id: BIGSERIAL）';


--
-- Name: COLUMN privchat_groups.qr_key; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public.privchat_groups.qr_key IS 'QR_CODE_SPEC v1.3: 群二维码 opaque token, 16 chars base62, UNIQUE, 永久，Owner/Admin 可 refresh 旋转';


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
-- Name: TABLE privchat_login_logs; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON TABLE public.privchat_login_logs IS '登录日志表：记录用户 token 首次认证的登录行为，关联到具体设备';


--
-- Name: COLUMN privchat_login_logs.device_id; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public.privchat_login_logs.device_id IS '设备ID（强关联），可追溯该设备的所有登录历史';


--
-- Name: COLUMN privchat_login_logs.token_jti; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public.privchat_login_logs.token_jti IS 'JWT ID，可用于关联 token 撤销记录和去重检测';


--
-- Name: COLUMN privchat_login_logs.token_first_used_at; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public.privchat_login_logs.token_first_used_at IS 'Token 首次认证时间（真正的登录时间）';


--
-- Name: COLUMN privchat_login_logs.risk_score; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public.privchat_login_logs.risk_score IS '风险评分 0-100，根据多种因素计算';


--
-- Name: COLUMN privchat_login_logs.risk_factors; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public.privchat_login_logs.risk_factors IS '风险因素数组，如 ["new_location", "unusual_time", "proxy_or_vpn"]';


--
-- Name: COLUMN privchat_login_logs.is_new_device; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public.privchat_login_logs.is_new_device IS '该设备是否为首次登录（device_id 首次出现）';


--
-- Name: COLUMN privchat_login_logs.is_new_location; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public.privchat_login_logs.is_new_location IS '该用户是否从新地理位置登录';


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
-- Name: TABLE privchat_messages; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON TABLE public.privchat_messages IS '消息表：存储所有消息（message_id: Snowflake uint64，分区表，按月分区）';


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


-- (privchat_migrations 表由 migrator 自管, 不在初始化 SQL 里建)
-- (privchat_migrations 表由 migrator 自管, 不在初始化 SQL 里建)
-- (privchat_migrations 表由 migrator 自管, 不在初始化 SQL 里建)
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
-- Name: TABLE privchat_offline_message_queue; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON TABLE public.privchat_offline_message_queue IS '离线消息队列：基于 pts 的轻量级队列';


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
-- Name: privchat_read_receipts; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.privchat_read_receipts (
    message_id bigint NOT NULL,
    user_id bigint NOT NULL,
    channel_id bigint NOT NULL,
    read_at bigint DEFAULT public.now_millis() NOT NULL
);


--
-- Name: TABLE privchat_read_receipts; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON TABLE public.privchat_read_receipts IS '已读回执表：存储消息已读状态';


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
-- Name: TABLE privchat_user_channels; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON TABLE public.privchat_user_channels IS '用户频道列表表：存储用户的频道列表视图';


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
-- Name: TABLE privchat_user_devices; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON TABLE public.privchat_user_devices IS '用户设备推送信息表（Phase 3.5: 包含推送状态字段）';


--
-- Name: COLUMN privchat_user_devices.user_id; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public.privchat_user_devices.user_id IS '用户ID';


--
-- Name: COLUMN privchat_user_devices.device_id; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public.privchat_user_devices.device_id IS '设备ID';


--
-- Name: COLUMN privchat_user_devices.platform; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public.privchat_user_devices.platform IS '平台：ios / android / desktop';


--
-- Name: COLUMN privchat_user_devices.vendor; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public.privchat_user_devices.vendor IS '推送平台：apns / fcm';


--
-- Name: COLUMN privchat_user_devices.push_token; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public.privchat_user_devices.push_token IS '推送令牌';


--
-- Name: COLUMN privchat_user_devices.apns_armed; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public.privchat_user_devices.apns_armed IS '是否需要推送（客户端声明能力）';


--
-- Name: COLUMN privchat_user_devices.connected; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public.privchat_user_devices.connected IS '是否存在可用长连接（QUIC/WebSocket/TCP，事实状态）';


--
-- Name: COLUMN privchat_user_devices.last_send_ts; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public.privchat_user_devices.last_send_ts IS '最近一次发送成功时间';


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
-- Name: TABLE privchat_user_last_seen; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON TABLE public.privchat_user_last_seen IS '用户最后上线时间表：存储用户最后活跃时间，保留30天数据';


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
-- Name: TABLE privchat_user_settings; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON TABLE public.privchat_user_settings IS '用户设置（ENTITY_SYNC_V1 user_settings），表为主';


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
-- Name: TABLE privchat_users; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON TABLE public.privchat_users IS '用户表：存储用户基本信息（user_id: BIGSERIAL）';


--
-- Name: COLUMN privchat_users.qr_key; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public.privchat_users.qr_key IS 'QR_CODE_SPEC v1.3: 个人名片码 opaque token, 16 chars base62, UNIQUE, 永久，用户可主动 refresh 旋转';


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
-- Name: SEQUENCE privchat_users_user_id_seq; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON SEQUENCE public.privchat_users_user_id_seq IS '用户 ID 序列：从 100000000 开始，1~99 保留给系统功能用户';


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
-- Name: privchat_groups group_id; Type: DEFAULT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.privchat_groups ALTER COLUMN group_id SET DEFAULT nextval('public.privchat_groups_group_id_seq'::regclass);


--
-- Name: privchat_login_logs log_id; Type: DEFAULT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.privchat_login_logs ALTER COLUMN log_id SET DEFAULT nextval('public.privchat_login_logs_log_id_seq'::regclass);


-- (privchat_migrations 表由 migrator 自管, 不在初始化 SQL 里建)
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
    ADD CONSTRAINT privchat_client_msg_registry_pkey PRIMARY KEY (local_message_id);


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
-- Name: privchat_group_members privchat_group_members_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.privchat_group_members
    ADD CONSTRAINT privchat_group_members_pkey PRIMARY KEY (group_id, user_id);


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


-- (privchat_migrations 表由 migrator 自管, 不在初始化 SQL 里建)
-- (privchat_migrations 表由 migrator 自管, 不在初始化 SQL 里建)
--
-- Name: privchat_offline_message_queue privchat_offline_message_queue_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.privchat_offline_message_queue
    ADD CONSTRAINT privchat_offline_message_queue_pkey PRIMARY KEY (id);


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
-- Name: idx_bot_follow_bot; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_bot_follow_bot ON public.privchat_bot_follow USING btree (bot_user_id, status);


--
-- Name: idx_bot_follow_user; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_bot_follow_user ON public.privchat_bot_follow USING btree (user_id, status);


--
-- Name: idx_message_reactions_message_id; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_message_reactions_message_id ON public.privchat_message_reactions USING btree (message_id);


--
-- Name: idx_message_reactions_user_id; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_message_reactions_user_id ON public.privchat_message_reactions USING btree (user_id);


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
-- Name: idx_privchat_file_uploads_business; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_privchat_file_uploads_business ON public.privchat_file_uploads USING btree (business_type, business_id);


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
-- Name: idx_privchat_messages_sender; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_privchat_messages_sender ON ONLY public.privchat_messages USING btree (sender_id, created_at DESC);


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
-- Name: privchat_messages_2026_01_metadata_idx; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX privchat_messages_2026_01_metadata_idx ON public.privchat_messages_2026_01 USING gin (metadata);


--
-- Name: privchat_messages_2026_01_reply_to_message_id_idx; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX privchat_messages_2026_01_reply_to_message_id_idx ON public.privchat_messages_2026_01 USING btree (reply_to_message_id) WHERE (reply_to_message_id IS NOT NULL);


--
-- Name: privchat_messages_2026_01_sender_id_created_at_idx; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX privchat_messages_2026_01_sender_id_created_at_idx ON public.privchat_messages_2026_01 USING btree (sender_id, created_at DESC);


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
-- Name: privchat_messages_2026_02_metadata_idx; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX privchat_messages_2026_02_metadata_idx ON public.privchat_messages_2026_02 USING gin (metadata);


--
-- Name: privchat_messages_2026_02_reply_to_message_id_idx; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX privchat_messages_2026_02_reply_to_message_id_idx ON public.privchat_messages_2026_02 USING btree (reply_to_message_id) WHERE (reply_to_message_id IS NOT NULL);


--
-- Name: privchat_messages_2026_02_sender_id_created_at_idx; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX privchat_messages_2026_02_sender_id_created_at_idx ON public.privchat_messages_2026_02 USING btree (sender_id, created_at DESC);


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
-- Name: privchat_messages_2026_03_metadata_idx; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX privchat_messages_2026_03_metadata_idx ON public.privchat_messages_2026_03 USING gin (metadata);


--
-- Name: privchat_messages_2026_03_reply_to_message_id_idx; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX privchat_messages_2026_03_reply_to_message_id_idx ON public.privchat_messages_2026_03 USING btree (reply_to_message_id) WHERE (reply_to_message_id IS NOT NULL);


--
-- Name: privchat_messages_2026_03_sender_id_created_at_idx; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX privchat_messages_2026_03_sender_id_created_at_idx ON public.privchat_messages_2026_03 USING btree (sender_id, created_at DESC);


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
-- Name: privchat_messages_2026_04_metadata_idx; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX privchat_messages_2026_04_metadata_idx ON public.privchat_messages_2026_04 USING gin (metadata);


--
-- Name: privchat_messages_2026_04_reply_to_message_id_idx; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX privchat_messages_2026_04_reply_to_message_id_idx ON public.privchat_messages_2026_04 USING btree (reply_to_message_id) WHERE (reply_to_message_id IS NOT NULL);


--
-- Name: privchat_messages_2026_04_sender_id_created_at_idx; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX privchat_messages_2026_04_sender_id_created_at_idx ON public.privchat_messages_2026_04 USING btree (sender_id, created_at DESC);


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
-- Name: privchat_messages_default_metadata_idx; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX privchat_messages_default_metadata_idx ON public.privchat_messages_default USING gin (metadata);


--
-- Name: privchat_messages_default_reply_to_message_id_idx; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX privchat_messages_default_reply_to_message_id_idx ON public.privchat_messages_default USING btree (reply_to_message_id) WHERE (reply_to_message_id IS NOT NULL);


--
-- Name: privchat_messages_default_sender_id_created_at_idx; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX privchat_messages_default_sender_id_created_at_idx ON public.privchat_messages_default USING btree (sender_id, created_at DESC);


--
-- Name: privchat_messages_default_sender_id_pts_idx; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX privchat_messages_default_sender_id_pts_idx ON public.privchat_messages_default USING btree (sender_id, pts);


--
-- Name: uk_bot_follow_user_bot; Type: INDEX; Schema: public; Owner: -
--

CREATE UNIQUE INDEX uk_bot_follow_user_bot ON public.privchat_bot_follow USING btree (user_id, bot_user_id);


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
-- Name: privchat_messages_2026_01_metadata_idx; Type: INDEX ATTACH; Schema: public; Owner: -
--

ALTER INDEX public.idx_privchat_messages_metadata_gin ATTACH PARTITION public.privchat_messages_2026_01_metadata_idx;


--
-- Name: privchat_messages_2026_01_pkey; Type: INDEX ATTACH; Schema: public; Owner: -
--

ALTER INDEX public.privchat_messages_pkey ATTACH PARTITION public.privchat_messages_2026_01_pkey;


--
-- Name: privchat_messages_2026_01_reply_to_message_id_idx; Type: INDEX ATTACH; Schema: public; Owner: -
--

ALTER INDEX public.idx_privchat_messages_reply ATTACH PARTITION public.privchat_messages_2026_01_reply_to_message_id_idx;


--
-- Name: privchat_messages_2026_01_sender_id_created_at_idx; Type: INDEX ATTACH; Schema: public; Owner: -
--

ALTER INDEX public.idx_privchat_messages_sender ATTACH PARTITION public.privchat_messages_2026_01_sender_id_created_at_idx;


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
-- Name: privchat_messages_2026_02_metadata_idx; Type: INDEX ATTACH; Schema: public; Owner: -
--

ALTER INDEX public.idx_privchat_messages_metadata_gin ATTACH PARTITION public.privchat_messages_2026_02_metadata_idx;


--
-- Name: privchat_messages_2026_02_pkey; Type: INDEX ATTACH; Schema: public; Owner: -
--

ALTER INDEX public.privchat_messages_pkey ATTACH PARTITION public.privchat_messages_2026_02_pkey;


--
-- Name: privchat_messages_2026_02_reply_to_message_id_idx; Type: INDEX ATTACH; Schema: public; Owner: -
--

ALTER INDEX public.idx_privchat_messages_reply ATTACH PARTITION public.privchat_messages_2026_02_reply_to_message_id_idx;


--
-- Name: privchat_messages_2026_02_sender_id_created_at_idx; Type: INDEX ATTACH; Schema: public; Owner: -
--

ALTER INDEX public.idx_privchat_messages_sender ATTACH PARTITION public.privchat_messages_2026_02_sender_id_created_at_idx;


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
-- Name: privchat_messages_2026_03_metadata_idx; Type: INDEX ATTACH; Schema: public; Owner: -
--

ALTER INDEX public.idx_privchat_messages_metadata_gin ATTACH PARTITION public.privchat_messages_2026_03_metadata_idx;


--
-- Name: privchat_messages_2026_03_pkey; Type: INDEX ATTACH; Schema: public; Owner: -
--

ALTER INDEX public.privchat_messages_pkey ATTACH PARTITION public.privchat_messages_2026_03_pkey;


--
-- Name: privchat_messages_2026_03_reply_to_message_id_idx; Type: INDEX ATTACH; Schema: public; Owner: -
--

ALTER INDEX public.idx_privchat_messages_reply ATTACH PARTITION public.privchat_messages_2026_03_reply_to_message_id_idx;


--
-- Name: privchat_messages_2026_03_sender_id_created_at_idx; Type: INDEX ATTACH; Schema: public; Owner: -
--

ALTER INDEX public.idx_privchat_messages_sender ATTACH PARTITION public.privchat_messages_2026_03_sender_id_created_at_idx;


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
-- Name: privchat_messages_2026_04_metadata_idx; Type: INDEX ATTACH; Schema: public; Owner: -
--

ALTER INDEX public.idx_privchat_messages_metadata_gin ATTACH PARTITION public.privchat_messages_2026_04_metadata_idx;


--
-- Name: privchat_messages_2026_04_pkey; Type: INDEX ATTACH; Schema: public; Owner: -
--

ALTER INDEX public.privchat_messages_pkey ATTACH PARTITION public.privchat_messages_2026_04_pkey;


--
-- Name: privchat_messages_2026_04_reply_to_message_id_idx; Type: INDEX ATTACH; Schema: public; Owner: -
--

ALTER INDEX public.idx_privchat_messages_reply ATTACH PARTITION public.privchat_messages_2026_04_reply_to_message_id_idx;


--
-- Name: privchat_messages_2026_04_sender_id_created_at_idx; Type: INDEX ATTACH; Schema: public; Owner: -
--

ALTER INDEX public.idx_privchat_messages_sender ATTACH PARTITION public.privchat_messages_2026_04_sender_id_created_at_idx;


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
-- Name: privchat_messages_default_metadata_idx; Type: INDEX ATTACH; Schema: public; Owner: -
--

ALTER INDEX public.idx_privchat_messages_metadata_gin ATTACH PARTITION public.privchat_messages_default_metadata_idx;


--
-- Name: privchat_messages_default_pkey; Type: INDEX ATTACH; Schema: public; Owner: -
--

ALTER INDEX public.privchat_messages_pkey ATTACH PARTITION public.privchat_messages_default_pkey;


--
-- Name: privchat_messages_default_reply_to_message_id_idx; Type: INDEX ATTACH; Schema: public; Owner: -
--

ALTER INDEX public.idx_privchat_messages_reply ATTACH PARTITION public.privchat_messages_default_reply_to_message_id_idx;


--
-- Name: privchat_messages_default_sender_id_created_at_idx; Type: INDEX ATTACH; Schema: public; Owner: -
--

ALTER INDEX public.idx_privchat_messages_sender ATTACH PARTITION public.privchat_messages_default_sender_id_created_at_idx;


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


-- ==============================================================================
-- 用户 ID 序列起始值
--   1 ~ 99: 系统功能用户保留 (系统消息 / 文件助手等, 不在表里)
--   100000000+: 普通用户 + 机器人 (用 user_type 区分)
-- ==============================================================================
DO $$
DECLARE user_count INTEGER;
BEGIN
    SELECT COUNT(*) INTO user_count FROM public.privchat_users;
    IF user_count = 0 THEN
        PERFORM setval('public.privchat_users_user_id_seq', 100000000, false);
        RAISE NOTICE 'privchat_users_user_id_seq starts at 100000000';
    END IF;
END$$;
