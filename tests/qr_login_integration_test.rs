// Copyright 2024 Shanghai Boyu Information Technology Co., Ltd.
// https://privchat.dev
//
// Author: zoujiaqing <zoujiaqing@gmail.com>
//
// Licensed under the Apache License, Version 2.0.

//! QR Login 进程内集成测试（spec QR_API §5、QR_LOGIN_SPEC §5）。
//!
//! 这层测试钉住的是 **`QrLoginService` ↔ `QrLoginPublisher` 的接线**：
//! scan / reject / tick_expired 这三个状态转换内部确实把对应事件下放到 publisher。
//! 跟 `qr_login_publisher::tests` 的纯结构单测不同 —— 那一层只测 publisher 自己；
//! 这一层测 service 转换是否真的触发 auto-push 钩子。
//!
//! 测试方法：
//! 1. 用真实 [`QrLoginPublisher`] + [`crate::infra::ConnectionManager::new()`] 拼出 service
//! 2. 手动调 [`QrLoginPublisher::bind`] 模拟 unauth 连接已建立（绕过 RPC handler）
//! 3. 触发 service 的状态转换
//! 4. 等 spawn_push 任务跑完
//! 5. 通过 publisher 的双向 registry 状态反推：
//!    - 终态 push（rejected / expired）→ binding 必须被 `unbind_by_scene` 清掉
//!    - 非终态 push（scanned）→ binding 必须**保留**
//! 6. 同时校验 service 自身状态机不因 push delivery 失败而回滚
//!
//! `ConnectionManager::new()` 没有 transport，`send_unauth_event_to_session` 会走
//! `transport.as_ref()` 为 None 的路径返回 `Ok(0)`，让 push_event 走 sent=0 + terminal
//! 后 unbind 的代码路径，正好对应"NoSubscriber"行为，无需真起 transport。

use std::sync::Arc;
use std::time::Duration;

use msgtrans::SessionId;

use privchat::infra::ConnectionManager;
use privchat::service::qr_login_service::{QrLoginService, QrSceneState, WebDeviceSnapshot};
use privchat::service::QrLoginPublisher;

fn make_device() -> WebDeviceSnapshot {
    WebDeviceSnapshot {
        device_name: Some("Chrome".into()),
        user_agent: Some("Mozilla/5.0".into()),
        ip_address: Some("203.0.113.10".into()),
    }
}

/// `spawn_push` 把推送丢进 tokio task；等几十毫秒让它跑完是合理的。
async fn settle() {
    tokio::time::sleep(Duration::from_millis(50)).await;
}

/// #1 / #3：scan_scene 转换内部 spawn push `qr_login.scanned`，**非终态**，
/// 即便 NoSubscriber 也不能 unbind（用户后续还会 confirm/reject）。
#[tokio::test]
async fn scan_transition_pushes_scanned_without_unbind() {
    let publisher = Arc::new(QrLoginPublisher::new());
    let cm = Arc::new(ConnectionManager::new());
    let service = QrLoginService::new().with_publisher(publisher.clone(), cm);

    let scene = service.create_scene("login".into(), "web-1".into(), make_device(), Some(60)).await;
    let session = SessionId::new(101);
    publisher.bind(scene.scene_id.clone(), session);
    assert_eq!(publisher.binding_count(), 1);

    let result = service
        .scan_scene(
            &scene.scene_id,
            100,
            "ios-1".into(),
            &scene.qr_token,
            None,
            Some("Alice".into()),
        )
        .await
        .expect("scan_scene must succeed");
    assert_eq!(result.scene.state, QrSceneState::Scanned);

    settle().await;

    // 非终态：binding 必须保留
    assert_eq!(
        publisher.binding_count(),
        1,
        "scanned 非终态，binding 不能被清掉"
    );
    assert_eq!(
        publisher.lookup_session(&scene.scene_id),
        Some(session),
        "binding 必须仍指向原 session"
    );
}

/// #4：reject_scene 转换 spawn push `qr_login.rejected`（**终态**），
/// 推送完 publisher 必须 `unbind_by_scene`，registry 必须双向清空。
#[tokio::test]
async fn reject_transition_pushes_rejected_and_unbinds() {
    let publisher = Arc::new(QrLoginPublisher::new());
    let cm = Arc::new(ConnectionManager::new());
    let service = QrLoginService::new().with_publisher(publisher.clone(), cm);

    let scene = service.create_scene("login".into(), "web-1".into(), make_device(), Some(60)).await;
    let session = SessionId::new(202);
    publisher.bind(scene.scene_id.clone(), session);

    // 必须先 scan 才能 reject
    let scan = service
        .scan_scene(&scene.scene_id, 100, "ios-1".into(), &scene.qr_token, None, None)
        .await
        .expect("scan");
    settle().await;
    assert_eq!(publisher.binding_count(), 1, "scanned 后 binding 还在");

    let rejected = service
        .reject_scene(&scene.scene_id, 100, &scan.confirm_token)
        .await
        .expect("reject");
    assert_eq!(rejected.state, QrSceneState::Rejected);

    settle().await;

    // 终态：binding 必须被清，且双向都清
    assert_eq!(
        publisher.binding_count(),
        0,
        "rejected 终态，scene_to_session 必须被清"
    );
    assert_eq!(
        publisher.lookup_session(&scene.scene_id),
        None,
        "scene→session 不能残留"
    );
    assert_eq!(
        publisher.unbind_by_session(&session),
        None,
        "session→scene 也必须被清（双向 registry 一致）"
    );
}

/// #6：tick_expired 把过期 scene 切到 expired 并 push `qr_login.expired`，
/// 终态 unbind。用 ttl=1s 触发真实过期判定，避免 mock 时间。
#[tokio::test]
async fn tick_expired_pushes_expired_and_unbinds() {
    let publisher = Arc::new(QrLoginPublisher::new());
    let cm = Arc::new(ConnectionManager::new());
    let service = QrLoginService::new().with_publisher(publisher.clone(), cm);

    // ttl=1s，到期最快
    let scene = service.create_scene("login".into(), "web-1".into(), make_device(), Some(1)).await;
    let session = SessionId::new(303);
    publisher.bind(scene.scene_id.clone(), session);

    // 等过 TTL 边界
    tokio::time::sleep(Duration::from_millis(1100)).await;

    service.tick_expired().await;

    // 状态机：scene 应已切到 expired
    let after = service
        .get_scene(&scene.scene_id)
        .await
        .expect("scene still queryable for status");
    assert_eq!(after.state, QrSceneState::Expired);

    // publisher：terminal push 后 binding 必须双向清
    assert_eq!(publisher.binding_count(), 0, "expired 终态必须 unbind");
    assert_eq!(publisher.lookup_session(&scene.scene_id), None);
    assert_eq!(publisher.unbind_by_session(&session), None);
}

/// 不应该被 reject 终态 unbind 误清的场景：reject_scene 校验失败（confirm_token
/// 不对），状态机回弹错误 —— 此时 publisher 不应该被触碰，binding 保持原样。
/// 这条防止 hook 被错位塞进失败分支。
#[tokio::test]
async fn reject_failure_does_not_touch_publisher() {
    let publisher = Arc::new(QrLoginPublisher::new());
    let cm = Arc::new(ConnectionManager::new());
    let service = QrLoginService::new().with_publisher(publisher.clone(), cm);

    let scene = service.create_scene("login".into(), "web-1".into(), make_device(), Some(60)).await;
    publisher.bind(scene.scene_id.clone(), SessionId::new(404));
    let _scan = service
        .scan_scene(&scene.scene_id, 100, "ios-1".into(), &scene.qr_token, None, None)
        .await
        .expect("scan");
    settle().await;
    assert_eq!(publisher.binding_count(), 1);

    // 错误的 confirm_token → reject 失败
    let err = service
        .reject_scene(&scene.scene_id, 100, "wrong-confirm-token")
        .await
        .unwrap_err();
    assert!(
        format!("{}", err).contains("QR_CONFIRM_TOKEN_INVALID"),
        "应该返回 token invalid 错误"
    );
    settle().await;

    // 校验失败不应触发 push、不应解绑
    assert_eq!(
        publisher.binding_count(),
        1,
        "reject 失败时 publisher 不应被触碰"
    );

    // 状态机也不应被改写
    let after = service.get_scene(&scene.scene_id).await.expect("query");
    assert_eq!(after.state, QrSceneState::Scanned, "状态机必须保留 scanned");
}

/// #8 spec QR_API §5：NoSubscriber 不应让 service 状态机回滚。
/// 这条等价于"publisher 没有 binding 时，service.scan_scene 仍然把状态切到 scanned"。
#[tokio::test]
async fn no_subscriber_does_not_rollback_state_machine() {
    let publisher = Arc::new(QrLoginPublisher::new());
    let cm = Arc::new(ConnectionManager::new());
    let service = QrLoginService::new().with_publisher(publisher.clone(), cm);

    let scene = service.create_scene("login".into(), "web-1".into(), make_device(), Some(60)).await;
    // 故意不 bind —— Web 还没建连接
    assert_eq!(publisher.binding_count(), 0);

    let r = service
        .scan_scene(&scene.scene_id, 100, "ios-1".into(), &scene.qr_token, None, None)
        .await
        .expect("scan_scene 必须成功，即便没有订阅者");
    settle().await;

    assert_eq!(r.scene.state, QrSceneState::Scanned);
    let after = service.get_scene(&scene.scene_id).await.expect("query");
    assert_eq!(
        after.state,
        QrSceneState::Scanned,
        "NoSubscriber 不应让状态机回滚到 Created"
    );

    // reject 也一样
    let rejected = service
        .reject_scene(&scene.scene_id, 100, &r.confirm_token)
        .await
        .expect("reject_scene 必须成功，即便没有订阅者");
    settle().await;
    assert_eq!(rejected.state, QrSceneState::Rejected);
}

/// 多个 scene 并存时各自独立：reject 一个 scene 的 binding 不会影响其他 scene。
#[tokio::test]
async fn concurrent_scenes_unbind_is_isolated() {
    let publisher = Arc::new(QrLoginPublisher::new());
    let cm = Arc::new(ConnectionManager::new());
    let service = QrLoginService::new().with_publisher(publisher.clone(), cm);

    let s1 = service.create_scene("login".into(), "web-1".into(), make_device(), Some(60)).await;
    let s2 = service.create_scene("login".into(), "web-2".into(), make_device(), Some(60)).await;
    publisher.bind(s1.scene_id.clone(), SessionId::new(501));
    publisher.bind(s2.scene_id.clone(), SessionId::new(502));
    assert_eq!(publisher.binding_count(), 2);

    // 都 scan，都 reject scene 1
    let scan1 = service
        .scan_scene(&s1.scene_id, 100, "ios-1".into(), &s1.qr_token, None, None)
        .await
        .unwrap();
    let _scan2 = service
        .scan_scene(&s2.scene_id, 200, "ios-2".into(), &s2.qr_token, None, None)
        .await
        .unwrap();
    settle().await;
    assert_eq!(publisher.binding_count(), 2);

    let _ = service
        .reject_scene(&s1.scene_id, 100, &scan1.confirm_token)
        .await
        .unwrap();
    settle().await;

    // 只有 s1 的 binding 没了
    assert_eq!(publisher.binding_count(), 1);
    assert_eq!(publisher.lookup_session(&s1.scene_id), None);
    assert_eq!(
        publisher.lookup_session(&s2.scene_id),
        Some(SessionId::new(502))
    );
}
