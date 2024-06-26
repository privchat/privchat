import 'dart:ui';

import 'package:flutter_openim_sdk/flutter_openim_sdk.dart';
import 'package:get/get.dart';
import 'package:privchat/core/controller/app_controller.dart';
import 'package:privchat/routes/app_navigator.dart';
import 'package:privchat_common/privchat_common.dart';

import '../../../core/controller/im_controller.dart';

class SystemSetupLogic extends GetxController {
  final imLogic = Get.find<IMController>();
  final appLogic = Get.find<AppController>();

  @override
  void onReady() {
    super.onReady();
  }

  @override
  void onClose() {
    super.onClose();
  }

  @override
  void onInit() {
    _queryMyFullInfo();
    super.onInit();
  }

  void _queryMyFullInfo() async {
    final data = await LoadingView.singleton.wrap(
      asyncFunction: () => Apis.queryMyFullInfo(),
    );
    if (data is UserFullInfo) {
      final userInfo = UserFullInfo.fromJson(data.toJson());
      imLogic.userInfo.update((val) {
        val?.allowAddFriend = userInfo.allowAddFriend;
        val?.allowBeep = userInfo.allowBeep;
        val?.allowVibration = userInfo.allowVibration;
      });
    }
  }

  void blacklist() => AppNavigator.startBlacklist();

  void clearChatHistory() async {
    var confirm = await Get.dialog(CustomDialog(
      title: StrRes.confirmClearChatHistory,
    ));
    if (confirm == true) {
      LoadingView.singleton.wrap(asyncFunction: () async {
        await OpenIM.iMManager.messageManager.deleteAllMsgFromLocalAndSvr();
      });
    }
  }

  void setLanguage(){
    Get.bottomSheet(
      BottomSheetView(
        items: [
          SheetItem(
            label: StrRes.chinese,
            onTap: () {
              DataSp.putLanguage(1);
              Get.updateLocale(Locale('zh','CN'));
            },
          ),
          SheetItem(
            label: StrRes.english,
            onTap: () {
              DataSp.putLanguage(2);
              Get.updateLocale(Locale('en','US'));
            },
          ),
        ],
      ),
    );
  }
}
