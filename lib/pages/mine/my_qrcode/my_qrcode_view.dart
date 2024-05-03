import 'package:flutter/material.dart';
import 'package:flutter_screenutil/flutter_screenutil.dart';
import 'package:get/get.dart';
import 'package:privchat_common/privchat_common.dart';
import 'package:qr_flutter/qr_flutter.dart';

import 'my_qrcode_logic.dart';

class MyQrcodePage extends StatelessWidget {
  final logic = Get.find<MyQrcodeLogic>();

  MyQrcodePage({super.key});

  @override
  Widget build(BuildContext context) {
    return Obx(
      () => Scaffold(
        appBar: TitleBar.back(
          title: StrRes.qrcode,
        ),
        backgroundColor: Styles.c_F8F9FA,
        body: Container(
          alignment: Alignment.topCenter,
          child: Container(
            margin: EdgeInsets.symmetric(horizontal: 20.w, vertical: 20.h),
            padding: EdgeInsets.symmetric(horizontal: 30.w),
            // width: 331.w,
            height: 460.h,
            decoration: BoxDecoration(
              color: Styles.c_FFFFFF,
              borderRadius: BorderRadius.circular(6.r),
              // boxShadow: [
              //   BoxShadow(
              //     blurRadius: 7.r,
              //     spreadRadius: 1.r,
              //     color: Styles.c_000000.withOpacity(.08),
              //   ),
              // ],
            ),
            child: Stack(
              children: [
                Positioned(
                  top: 30.h,
                  child: Row(
                    crossAxisAlignment: CrossAxisAlignment.center,
                    children: [
                      AvatarView(
                        width: 48.w,
                        height: 48.h,
                        url: logic.imLogic.userInfo.value.faceURL,
                        text: logic.imLogic.userInfo.value.nickname,
                        textStyle: Styles.ts_FFFFFF_14sp,
                      ),
                      12.horizontalSpace,
                      (logic.imLogic.userInfo.value.nickname ?? '').toText
                        ..style = Styles.ts_0C1C33_20sp,
                    ],
                  ),
                ),
                Positioned(
                  top: 100.h,
                  width: 272.w,
                  child: StrRes.qrcodeHint.toText
                    ..style = Styles.ts_8E9AB0_15sp
                    ..textAlign = TextAlign.center,
                ),
                Positioned(
                  top: 183.h,
                  width: 272.w,
                  child: Container(
                    alignment: Alignment.center,
                    child: Container(
                      width: 240.w,
                      height: 240.w,
                      alignment: Alignment.center,
                      child: QrImageView(
                        data: logic.buildQRContent(),
                        size: 240.w,
                        backgroundColor: Styles.c_FFFFFF,
                      ),
                    ),
                  ),
                )
              ],
            ),
          ),
        ),
      ),
    );
  }
}
