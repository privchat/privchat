import 'dart:async';
import 'dart:convert';
import 'dart:io';

import 'package:collection/collection.dart';
import 'package:flutter/services.dart';
import 'package:flutter_openim_sdk/flutter_openim_sdk.dart';
import 'package:get/get.dart';
import 'package:just_audio/just_audio.dart';
import 'package:privchat_common/privchat_common.dart';
import 'package:rxdart/rxdart.dart';
import 'package:uuid/uuid.dart';

import '../privchat_live.dart';

/// 信令
mixin OpenIMLive {
  final signalingSubject = PublishSubject<CallEvent>();

  void invitationCancelled(SignalingInfo info) {
    signalingSubject.add(CallEvent(CallState.beCanceled, info));
  }

  void inviteeAccepted(SignalingInfo info) {
    signalingSubject.add(CallEvent(CallState.beAccepted, info));
  }

  void inviteeRejected(SignalingInfo info) {
    signalingSubject.add(CallEvent(CallState.beRejected, info));
  }

  void receiveNewInvitation(SignalingInfo info) {
    signalingSubject.add(CallEvent(CallState.beCalled, info));
  }

  void beHangup(SignalingInfo info) {
    signalingSubject.add(CallEvent(CallState.beHangup, info));
  }

  final backgroundSubject = PublishSubject<bool>();

  Function(SignalingMessageEvent)? onSignalingMessage;
  final roomParticipantDisconnectedSubject = PublishSubject<RoomCallingInfo>();
  final roomParticipantConnectedSubject = PublishSubject<RoomCallingInfo>();

  bool _isRunningBackground = false;

  CallEvent? _beCalledEvent;

  bool _autoPickup = false;

  final _ring = 'assets/audio/live_ring.wav';
  final _audioPlayer = AudioPlayer(
    // Handle audio_session events ourselves for the purpose of this demo.
    handleInterruptions: false,
    // androidApplyAudioAttributes: false,
    // handleAudioSessionActivation: false,
  );

  bool get isBusy => OpenIMLiveClient().isBusy;

  onCloseLive() {
    signalingSubject.close();
    backgroundSubject.close();
    roomParticipantDisconnectedSubject.close();
    roomParticipantConnectedSubject.close();
    _stopSound();
  }

  onInitLive() async {
    _signalingListener();
    _insertSignalingMessageListener();
    backgroundSubject.listen((background) {
      _isRunningBackground = background;
      if (!_isRunningBackground) {
        if (_beCalledEvent != null) {
          signalingSubject.add(_beCalledEvent!);
        }
      }
    });

    roomParticipantDisconnectedSubject.listen((info) {
      if (null == info.participant || info.participant!.length == 1) {
        OpenIMLiveClient().closeByRoomID(info.invitation!.roomID!);
      }
    });
  }

  Stream<CallEvent> get _stream => signalingSubject.stream /*.where((event) => LiveClient.dispatchSignaling(event))*/;

  _signalingListener() => _stream.listen(
        (event) async {
          _beCalledEvent = null;
          if (event.state == CallState.beCalled) {
            _playSound();
            final mediaType = event.data.invitation!.mediaType;
            final sessionType = event.data.invitation!.sessionType;
            final callType = mediaType == 'audio' ? CallType.audio : CallType.video;
            final callObj = sessionType == ConversationType.single ? CallObj.single : CallObj.group;

            if (Platform.isAndroid && _isRunningBackground) {
              _beCalledEvent = event;
              if (await Permissions.checkSystemAlertWindow()) {
                return;
              }
            }
            _beCalledEvent = null;
            OpenIMLiveClient().start(
              Get.overlayContext!,
              callEventSubject: signalingSubject,
              roomID: event.data.invitation!.roomID!,
              inviteeUserIDList: event.data.invitation!.inviteeUserIDList!,
              inviterUserID: event.data.invitation!.inviterUserID!,
              groupID: event.data.invitation!.groupID,
              callType: callType,
              callObj: callObj,
              initState: CallState.beCalled,
              onSyncUserInfo: onSyncUserInfo,
              onSyncGroupInfo: onSyncGroupInfo,
              onSyncGroupMemberInfo: onSyncGroupMemberInfo,
              autoPickup: _autoPickup,
              onTapPickup: () => onTapPickup(
                event.data..userID = OpenIM.iMManager.userID,
              ),
              onTapReject: () => onTapReject(
                event.data..userID = OpenIM.iMManager.userID,
              ),
              onTapHangup: (duration, isPositive) => onTapHangup(
                event.data..userID = OpenIM.iMManager.userID,
                duration,
                isPositive,
              ),
              onError: onError,
              onRoomDisconnected: () => onRoomDisconnected(event.data),
            );
          } else if (event.state == CallState.beRejected) {
            _stopSound();
          } else if (event.state == CallState.beHangup) {
            _stopSound();
          } else if (event.state == CallState.beCanceled) {
            if (_isRunningBackground) {}
            _stopSound();
          } else if (event.state == CallState.beAccepted) {
            _stopSound();
          } else if (event.state == CallState.otherReject || event.state == CallState.otherAccepted) {
            _stopSound();
          } else if (event.state == CallState.timeout) {
            _stopSound();
            final sessionType = event.data.invitation!.sessionType;

            if (sessionType == 1) {
              onTimeoutCancelled(event.data);
            }
          }
        },
      );

  _insertSignalingMessageListener() {}

  call({
    required CallObj callObj,
    required CallType callType,
    CallState callState = CallState.call,
    String? roomID,
    String? inviterUserID,
    required List<String> inviteeUserIDList,
    String? groupID,
    SignalingCertificate? credentials,
  }) async {
    final mediaType = callType == CallType.audio ? 'audio' : 'video';
    final sessionType = callObj == CallObj.single ? 1 : 3;
    inviterUserID ??= OpenIM.iMManager.userID;

    final signal = SignalingInfo(
      userID: inviterUserID,
      invitation: InvitationInfo(
        inviterUserID: inviterUserID,
        inviteeUserIDList: inviteeUserIDList,
        roomID: roomID ?? groupID ?? const Uuid().v4(),
        timeout: 30,
        mediaType: mediaType,
        sessionType: sessionType,
        platformID: IMUtils.getPlatform(),
        groupID: groupID,
      ),
    );

    OpenIMLiveClient().start(
      Get.overlayContext!,
      callEventSubject: signalingSubject,
      inviterUserID: inviterUserID,
      groupID: groupID,
      inviteeUserIDList: inviteeUserIDList,
      callObj: callObj,
      callType: callType,
      initState: callState,
      onDialSingle: () => onDialSingle(signal),
      onJoinGroup: () => Future.value(credentials!),
      onTapCancel: () => onTapCancel(signal),
      onTapHangup: (duration, isPositive) => onTapHangup(
        signal,
        duration,
        isPositive,
      ),
      onSyncUserInfo: onSyncUserInfo,
      onSyncGroupInfo: onSyncGroupInfo,
      onSyncGroupMemberInfo: onSyncGroupMemberInfo,
      onWaitingAccept: () {
        if (callObj == CallObj.single) _playSound();
      },
      onBusyLine: onBusyLine,
      onStartCalling: () {
        _stopSound();
      },
      onError: onError,
      onRoomDisconnected: () => onRoomDisconnected(signal),
      onClose: _stopSound,
    );
  }

  onError(error, stack) {
    Logger.print('onError=====> $error $stack');
    OpenIMLiveClient().close();
    _stopSound();
    if (error is PlatformException) {
      if (int.parse(error.code) == SDKErrorCode.hasBeenBlocked) {
        IMViews.showToast(StrRes.callFail);
        return;
      }
    }
    IMViews.showToast(StrRes.networkError);
  }

  onRoomDisconnected(SignalingInfo signalingInfo) {}

  Future<SignalingCertificate> onDialSingle(SignalingInfo signaling) async {
    final data = {'customType': CustomMessageType.callingInvite, 'data': signaling.invitation!.toJson()};
    final message = await OpenIM.iMManager.messageManager.createCustomMessage(data: jsonEncode(data), extension: '', description: '');
    OpenIM.iMManager.messageManager
        .sendMessage(message: message, offlinePushInfo: OfflinePushInfo(), userID: signaling.invitation!.inviteeUserIDList!.first);
    final certificate = await Apis.getTokenForRTC(signaling.invitation!.roomID!, OpenIM.iMManager.userID);

    return certificate;
  }

  Future<SignalingCertificate> onTapPickup(SignalingInfo signaling) async {
    _beCalledEvent = null; // ios bug
    _autoPickup = false;
    _stopSound();
    final data = {'customType': CustomMessageType.callingAccept, 'data': signaling.invitation!.toJson()};
    final message = await OpenIM.iMManager.messageManager.createCustomMessage(data: jsonEncode(data), extension: '', description: '');
    OpenIM.iMManager.messageManager.sendMessage(message: message, offlinePushInfo: OfflinePushInfo(), userID: signaling.invitation!.inviterUserID);
    final certificate = await Apis.getTokenForRTC(signaling.invitation!.roomID!, OpenIM.iMManager.userID);

    return certificate;
  }

  onTapReject(SignalingInfo signaling) async {
    _stopSound();
    final data = {'customType': CustomMessageType.callingReject, 'data': signaling.invitation!.toJson()};
    final message = await OpenIM.iMManager.messageManager.createCustomMessage(data: jsonEncode(data), extension: '', description: '');
    final recvUserID = signaling.invitation!.inviterUserID == OpenIM.iMManager.userID
        ? signaling.invitation!.inviteeUserIDList!.first
        : signaling.invitation!.inviterUserID;
    return OpenIM.iMManager.messageManager.sendMessage(message: message, offlinePushInfo: OfflinePushInfo(), userID: recvUserID);
  }

  onTapCancel(SignalingInfo signaling) async {
    _stopSound();
    final data = {'customType': CustomMessageType.callingCancel, 'data': signaling.invitation!.toJson()};
    final message = await OpenIM.iMManager.messageManager.createCustomMessage(data: jsonEncode(data), extension: '', description: '');
    final recvUserID = signaling.invitation!.inviterUserID == OpenIM.iMManager.userID
        ? signaling.invitation!.inviteeUserIDList!.first
        : signaling.invitation!.inviterUserID;
    OpenIM.iMManager.messageManager.sendMessage(message: message, offlinePushInfo: OfflinePushInfo(), userID: recvUserID);
    return true;
  }

  onTimeoutCancelled(SignalingInfo signaling) async {
    final data = {'customType': CustomMessageType.callingCancel, 'data': signaling.invitation!.toJson()};
    final message = await OpenIM.iMManager.messageManager.createCustomMessage(data: jsonEncode(data), extension: '', description: '');

    OpenIM.iMManager.messageManager.sendMessage(message: message, offlinePushInfo: OfflinePushInfo(), userID: signaling.invitation!.inviterUserID);

    return true;
  }

  onTapHangup(SignalingInfo signaling, int duration, bool isPositive) async {
    if (isPositive) {
      final data = {'customType': CustomMessageType.callingHungup, 'data': signaling.invitation!.toJson()};
      final message = await OpenIM.iMManager.messageManager.createCustomMessage(data: jsonEncode(data), extension: '', description: '');
      final recvUserID = signaling.invitation!.inviterUserID == OpenIM.iMManager.userID
          ? signaling.invitation!.inviteeUserIDList!.first
          : signaling.invitation!.inviterUserID;
      OpenIM.iMManager.messageManager.sendMessage(message: message, offlinePushInfo: OfflinePushInfo(), userID: recvUserID);
    }
    _stopSound();
  }

  onBusyLine() {
    _stopSound();
    IMViews.showToast('用户正忙，请稍后再试！');
  }

  onJoin() {}

  Future<UserInfo?> onSyncUserInfo(userID) async {
    var list = await OpenIM.iMManager.userManager.getUsersInfo(
      userIDList: [userID],
    );

    return list.firstOrNull?.simpleUserInfo;
  }

  Future<GroupInfo?> onSyncGroupInfo(groupID) async {
    var list = await OpenIM.iMManager.groupManager.getGroupsInfo(
      groupIDList: [groupID],
    );
    return list.firstOrNull;
  }

  Future<List<GroupMembersInfo>> onSyncGroupMemberInfo(groupID, userIDList) async {
    var list = await OpenIM.iMManager.groupManager.getGroupMembersInfo(
      groupID: groupID,
      userIDList: userIDList,
    );
    return list;
  }

  void _playSound() async {
    if (!_audioPlayer.playerState.playing) {
      _audioPlayer.setAsset(_ring, package: 'privchat_common');
      _audioPlayer.setLoopMode(LoopMode.one);
      _audioPlayer.setVolume(1.0);
      _audioPlayer.play();
    }
  }

  void _stopSound() async {
    if (_audioPlayer.playerState.playing) {
      _audioPlayer.stop();
    }
  }

  void _recordCall({
    required CallState state,
    required SignalingInfo signaling,
    int duration = 0,
  }) async {}
}

class SignalingMessageEvent {
  Message message;
  String? userID;
  String? groupID;
  int sessionType;

  SignalingMessageEvent(
    this.message,
    this.sessionType,
    this.userID,
    this.groupID,
  );

  bool get isSingleChat => sessionType == ConversationType.single;

  bool get isGroupChat => sessionType == ConversationType.group || sessionType == ConversationType.superGroup;
}
