import 'package:flutter/cupertino.dart';
import 'package:flutter/material.dart';
import 'package:flutter_screenutil/flutter_screenutil.dart';
import 'package:get/get.dart';
import 'package:privchat_common/privchat_common.dart';

import 'account_setup_logic.dart';

class AccountSetupPage extends StatelessWidget {
  final logic = Get.find<AccountSetupLogic>();

  AccountSetupPage({super.key});

  @override
  Widget build(BuildContext context) {
    return Scaffold(
      appBar: TitleBar.back(
        title: StrRes.accountSetup,
      ),
      backgroundColor: Styles.c_F8F9FA,
      body: SingleChildScrollView(
        child: Column(
          children: [
            _buildChatOptionView(),
            10.verticalSpace,
            _buildItemView(
              text: StrRes.blacklist,
              onTap: logic.blacklist,
              showRightArrow: true,
            ),
            _buildChatOptionView(),
            10.verticalSpace,
            _buildItemView(
              text: StrRes.clearChatHistory,
              textStyle: Styles.ts_FF381F_17sp,
              onTap: logic.clearChatHistory,
              showRightArrow: true,
              isBottomRadius: true,
            ),
          ],
        ),
      ),
    );
  }


Widget _buildChatOptionView() => Container(
  decoration: BoxDecoration(
    color: Styles.c_FFFFFF,
    borderRadius: BorderRadius.circular(6.r),
  ),
  margin: EdgeInsets.symmetric(horizontal: 10.w, vertical: 10.h),
  child: Column(
    children: [
      _buildItemView(
        text: StrRes.topChat,
        isBottomRadius: true,
        showSwitchButton: true,
        // switchOn: logic.chatLogic.conversationInfo.value.isPinned!,
        // onTap: () => logic.chatLogic.setConversationTop(!logic.chatLogic.conversationInfo.value.isPinned!),
        // onChanged: (newValue) {
        //   logic.chatLogic.setConversationTop(newValue);
        //   // 刷新界面以更新开关状态
        //   setState(() {});
        // }
      ),
      _buildItemView(
        text: StrRes.messageNotDisturb,
        isBottomRadius: true,
        showSwitchButton: true,
        // switchOn: logic.chatLogic.conversationInfo.value.recvMsgOpt == 0 ? false : true,
        // onTap: () => logic.chatLogic.setConversationDisturb(logic.chatLogic.conversationInfo.value.recvMsgOpt == 0 ? 1 : 0),
        // onChanged: (newValue) {
        //   logic.chatLogic.setConversationTop(newValue);
        //   // 刷新界面以更新开关状态
        //   setState(() {});
        // }
      ),
    ]
  ),
);

Widget _buildItemView({
  required String text,
  TextStyle? textStyle,
  String? value,
  bool switchOn = false,
  bool isTopRadius = false,
  bool isBottomRadius = false,
  bool showRightArrow = false,
  bool showSwitchButton = false,
  ValueChanged<bool>? onChanged,
  Function()? onTap,
}) =>
  GestureDetector(
    onTap: onTap,
    behavior: HitTestBehavior.translucent,
    child: Container(
      height: 46.h,
      margin: EdgeInsets.symmetric(horizontal: 10.w),
      padding: EdgeInsets.symmetric(horizontal: 16.w),
      decoration: BoxDecoration(
        color: Styles.c_FFFFFF,
        borderRadius: BorderRadius.only(
          topRight: Radius.circular(isTopRadius ? 6.r : 0),
          topLeft: Radius.circular(isTopRadius ? 6.r : 0),
          bottomLeft: Radius.circular(isBottomRadius ? 6.r : 0),
          bottomRight: Radius.circular(isBottomRadius ? 6.r : 0),
        ),
      ),
      child: Row(
        children: [
          text.toText..style = textStyle ?? Styles.ts_0C1C33_17sp,
          const Spacer(),
          if (null != value) value.toText..style = Styles.ts_8E9AB0_14sp,
          if (showSwitchButton)
            CupertinoSwitch(
              value: switchOn,
              activeColor: Styles.c_0089FF,
              onChanged: onChanged,
            ),
          if (showRightArrow)
            ImageRes.rightArrow.toImage
              ..width = 24.w
              ..height = 24.h,
        ],
      ),
    ),
  );
}

