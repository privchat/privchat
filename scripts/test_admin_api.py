#!/usr/bin/env python3
"""
管理 API 测试脚本

用于测试 privchat-server 的管理接口，包括：
- 创建用户
- 创建群组
- 添加好友关系
- 拉人进群
- 查询统计信息等

使用方法:
    python3 scripts/test_admin_api.py
"""

import requests
import json
import sys
from typing import Dict, List, Optional, Tuple
from dataclasses import dataclass
from datetime import datetime

# 配置
BASE_URL = "http://localhost:8083"  # HTTP 文件服务器端口（从服务器日志中获取）
# 默认 service key（如果 .env 中没有设置 SERVICE_MASTER_KEY，服务器会使用这个默认值）
SERVICE_KEY = "default-service-master-key-please-change-in-production"

# 颜色输出
class Colors:
    GREEN = '\033[92m'
    RED = '\033[91m'
    YELLOW = '\033[93m'
    BLUE = '\033[94m'
    RESET = '\033[0m'

@dataclass
class TestResult:
    """测试结果"""
    name: str
    success: bool
    message: str
    data: Optional[Dict] = None
    error: Optional[str] = None

class AdminAPITester:
    """管理 API 测试器"""
    
    def __init__(self, base_url: str, service_key: str):
        self.base_url = base_url.rstrip('/')
        self.service_key = service_key
        self.headers = {
            "X-Service-Key": service_key,
            "Content-Type": "application/json"
        }
        self.created_users: List[int] = []
        self.created_groups: List[int] = []
        self.created_friendships: List[Tuple[int, int]] = []
        self.test_results: List[TestResult] = []
    
    def log(self, message: str, color: str = Colors.RESET):
        """打印日志"""
        print(f"{color}{message}{Colors.RESET}")
    
    def log_success(self, message: str):
        """打印成功消息"""
        self.log(f"✅ {message}", Colors.GREEN)
    
    def log_error(self, message: str):
        """打印错误消息"""
        self.log(f"❌ {message}", Colors.RED)
    
    def log_info(self, message: str):
        """打印信息消息"""
        self.log(f"ℹ️  {message}", Colors.BLUE)
    
    def log_warning(self, message: str):
        """打印警告消息"""
        self.log(f"⚠️  {message}", Colors.YELLOW)
    
    def make_request(self, method: str, endpoint: str, data: Optional[Dict] = None) -> Tuple[bool, Dict, Optional[str]]:
        """发送 HTTP 请求"""
        url = f"{self.base_url}{endpoint}"
        try:
            if method.upper() == "GET":
                response = requests.get(url, headers=self.headers, params=data)
            elif method.upper() == "POST":
                response = requests.post(url, headers=self.headers, json=data)
            elif method.upper() == "PUT":
                response = requests.put(url, headers=self.headers, json=data)
            elif method.upper() == "DELETE":
                response = requests.delete(url, headers=self.headers)
            else:
                return False, {}, f"不支持的 HTTP 方法: {method}"
            
            response.raise_for_status()
            return True, response.json(), None
        except requests.exceptions.RequestException as e:
            error_msg = f"请求失败: {str(e)}"
            if hasattr(e.response, 'text'):
                error_msg += f" - {e.response.text}"
            return False, {}, error_msg
    
    def test_create_user(self, username: str, display_name: Optional[str] = None, 
                        email: Optional[str] = None, phone: Optional[str] = None) -> Optional[int]:
        """测试创建用户"""
        self.log_info(f"创建用户: username={username}")
        
        data = {
            "username": username,
            "display_name": display_name,
            "email": email,
            "phone": phone
        }
        
        success, result, error = self.make_request("POST", "/api/admin/users", data)
        
        if success and result.get("success"):
            user_id = result.get("user_id")
            self.created_users.append(user_id)
            self.log_success(f"用户创建成功: user_id={user_id}, username={username}")
            self.test_results.append(TestResult(
                name=f"创建用户: {username}",
                success=True,
                message=f"用户ID: {user_id}",
                data=result
            ))
            return user_id
        else:
            self.log_error(f"创建用户失败: {error or result.get('message', '未知错误')}")
            self.test_results.append(TestResult(
                name=f"创建用户: {username}",
                success=False,
                message=error or "创建失败",
                error=error
            ))
            return None
    
    def test_list_users(self, page: int = 1, page_size: int = 20) -> bool:
        """测试获取用户列表"""
        self.log_info(f"获取用户列表: page={page}, page_size={page_size}")
        
        params = {"page": page, "page_size": page_size}
        success, result, error = self.make_request("GET", "/api/admin/users", params)
        
        if success:
            users = result.get("users", [])
            total = result.get("total", 0)
            self.log_success(f"获取用户列表成功: 总数={total}, 当前页={len(users)}")
            self.test_results.append(TestResult(
                name="获取用户列表",
                success=True,
                message=f"总数: {total}",
                data=result
            ))
            return True
        else:
            self.log_error(f"获取用户列表失败: {error}")
            self.test_results.append(TestResult(
                name="获取用户列表",
                success=False,
                message=error or "获取失败",
                error=error
            ))
            return False
    
    def test_get_user(self, user_id: int) -> bool:
        """测试获取用户详情"""
        self.log_info(f"获取用户详情: user_id={user_id}")
        
        success, result, error = self.make_request("GET", f"/api/admin/users/{user_id}")
        
        if success:
            username = result.get("username", "未知")
            self.log_success(f"获取用户详情成功: username={username}")
            self.test_results.append(TestResult(
                name=f"获取用户详情: {user_id}",
                success=True,
                message=f"用户名: {username}",
                data=result
            ))
            return True
        else:
            self.log_error(f"获取用户详情失败: {error}")
            self.test_results.append(TestResult(
                name=f"获取用户详情: {user_id}",
                success=False,
                message=error or "获取失败",
                error=error
            ))
            return False
    
    def test_create_friendship(self, user1_id: int, user2_id: int) -> bool:
        """测试创建好友关系"""
        self.log_info(f"创建好友关系: {user1_id} <-> {user2_id}")
        
        data = {
            "user1_id": user1_id,
            "user2_id": user2_id
        }
        
        success, result, error = self.make_request("POST", "/api/admin/friendships", data)
        
        if success and result.get("success"):
            channel_id = result.get("channel_id")
            self.created_friendships.append((user1_id, user2_id))
            self.log_success(f"好友关系创建成功: channel_id={channel_id}")
            self.test_results.append(TestResult(
                name=f"创建好友关系: {user1_id} <-> {user2_id}",
                success=True,
                message=f"会话ID: {channel_id}",
                data=result
            ))
            return True
        else:
            self.log_error(f"创建好友关系失败: {error or result.get('message', '未知错误')}")
            self.test_results.append(TestResult(
                name=f"创建好友关系: {user1_id} <-> {user2_id}",
                success=False,
                message=error or "创建失败",
                error=error
            ))
            return False
    
    def test_list_friendships(self, page: int = 1, page_size: int = 20) -> bool:
        """测试获取好友关系列表"""
        self.log_info(f"获取好友关系列表: page={page}, page_size={page_size}")
        
        params = {"page": page, "page_size": page_size}
        success, result, error = self.make_request("GET", "/api/admin/friendships", params)
        
        if success:
            friendships = result.get("friendships", [])
            total = result.get("total", 0)
            self.log_success(f"获取好友关系列表成功: 总数={total}, 当前页={len(friendships)}")
            self.test_results.append(TestResult(
                name="获取好友关系列表",
                success=True,
                message=f"总数: {total}",
                data=result
            ))
            return True
        else:
            self.log_error(f"获取好友关系列表失败: {error}")
            self.test_results.append(TestResult(
                name="获取好友关系列表",
                success=False,
                message=error or "获取失败",
                error=error
            ))
            return False
    
    def test_get_user_friends(self, user_id: int) -> bool:
        """测试获取用户的好友列表"""
        self.log_info(f"获取用户好友列表: user_id={user_id}")
        
        success, result, error = self.make_request("GET", f"/api/admin/friendships/{user_id}")
        
        if success:
            friends = result.get("friends", [])
            total = result.get("total", 0)
            self.log_success(f"获取用户好友列表成功: 好友数={total}")
            self.test_results.append(TestResult(
                name=f"获取用户好友列表: {user_id}",
                success=True,
                message=f"好友数: {total}",
                data=result
            ))
            return True
        else:
            self.log_error(f"获取用户好友列表失败: {error}")
            self.test_results.append(TestResult(
                name=f"获取用户好友列表: {user_id}",
                success=False,
                message=error or "获取失败",
                error=error
            ))
            return False
    
    def test_list_groups(self, page: int = 1, page_size: int = 20) -> bool:
        """测试获取群组列表"""
        self.log_info(f"获取群组列表: page={page}, page_size={page_size}")
        
        params = {"page": page, "page_size": page_size}
        success, result, error = self.make_request("GET", "/api/admin/groups", params)
        
        if success:
            groups = result.get("groups", [])
            total = result.get("total", 0)
            self.log_success(f"获取群组列表成功: 总数={total}, 当前页={len(groups)}")
            self.test_results.append(TestResult(
                name="获取群组列表",
                success=True,
                message=f"总数: {total}",
                data=result
            ))
            return True
        else:
            self.log_error(f"获取群组列表失败: {error}")
            self.test_results.append(TestResult(
                name="获取群组列表",
                success=False,
                message=error or "获取失败",
                error=error
            ))
            return False
    
    def test_get_group(self, group_id: int) -> bool:
        """测试获取群组详情"""
        self.log_info(f"获取群组详情: group_id={group_id}")
        
        success, result, error = self.make_request("GET", f"/api/admin/groups/{group_id}")
        
        if success:
            name = result.get("name", "未知")
            member_count = result.get("member_count", 0)
            self.log_success(f"获取群组详情成功: name={name}, member_count={member_count}")
            self.test_results.append(TestResult(
                name=f"获取群组详情: {group_id}",
                success=True,
                message=f"群组名: {name}, 成员数: {member_count}",
                data=result
            ))
            return True
        else:
            self.log_error(f"获取群组详情失败: {error}")
            self.test_results.append(TestResult(
                name=f"获取群组详情: {group_id}",
                success=False,
                message=error or "获取失败",
                error=error
            ))
            return False
    
    def test_get_stats(self) -> bool:
        """测试获取系统统计"""
        self.log_info("获取系统统计")
        
        success, result, error = self.make_request("GET", "/api/admin/stats")
        
        if success:
            users = result.get("users", {}).get("total", 0)
            groups = result.get("groups", {}).get("total", 0)
            messages = result.get("messages", {}).get("total", 0)
            devices = result.get("devices", {}).get("total", 0)
            self.log_success(f"获取系统统计成功: 用户={users}, 群组={groups}, 消息={messages}, 设备={devices}")
            self.test_results.append(TestResult(
                name="获取系统统计",
                success=True,
                message=f"用户: {users}, 群组: {groups}, 消息: {messages}, 设备: {devices}",
                data=result
            ))
            return True
        else:
            self.log_error(f"获取系统统计失败: {error}")
            self.test_results.append(TestResult(
                name="获取系统统计",
                success=False,
                message=error or "获取失败",
                error=error
            ))
            return False
    
    def run_all_tests(self):
        """运行所有测试"""
        self.log("\n" + "="*60, Colors.BLUE)
        self.log("开始运行管理 API 测试", Colors.BLUE)
        self.log("="*60 + "\n", Colors.BLUE)
        
        # 1. 创建多个用户
        self.log("\n--- 测试 1: 创建用户 ---", Colors.YELLOW)
        user1_id = self.test_create_user("test_user_1", "测试用户1", "user1@test.com", "13800138001")
        user2_id = self.test_create_user("test_user_2", "测试用户2", "user2@test.com", "13800138002")
        user3_id = self.test_create_user("test_user_3", "测试用户3", "user3@test.com", "13800138003")
        user4_id = self.test_create_user("test_user_4", "测试用户4", "user4@test.com", "13800138004")
        
        if not all([user1_id, user2_id, user3_id, user4_id]):
            self.log_error("用户创建失败，终止测试")
            return
        
        # 2. 获取用户列表
        self.log("\n--- 测试 2: 获取用户列表 ---", Colors.YELLOW)
        self.test_list_users(page=1, page_size=10)
        
        # 3. 获取用户详情
        self.log("\n--- 测试 3: 获取用户详情 ---", Colors.YELLOW)
        self.test_get_user(user1_id)
        
        # 4. 创建好友关系
        self.log("\n--- 测试 4: 创建好友关系 ---", Colors.YELLOW)
        self.test_create_friendship(user1_id, user2_id)
        self.test_create_friendship(user1_id, user3_id)
        self.test_create_friendship(user2_id, user3_id)
        self.test_create_friendship(user3_id, user4_id)
        
        # 5. 获取好友关系列表
        self.log("\n--- 测试 5: 获取好友关系列表 ---", Colors.YELLOW)
        self.test_list_friendships(page=1, page_size=10)
        
        # 6. 获取用户的好友列表
        self.log("\n--- 测试 6: 获取用户的好友列表 ---", Colors.YELLOW)
        self.test_get_user_friends(user1_id)
        self.test_get_user_friends(user2_id)
        
        # 7. 获取群组列表
        self.log("\n--- 测试 7: 获取群组列表 ---", Colors.YELLOW)
        self.test_list_groups(page=1, page_size=10)
        
        # 8. 获取系统统计
        self.log("\n--- 测试 8: 获取系统统计 ---", Colors.YELLOW)
        self.test_get_stats()
        
        # 打印测试总结
        self.print_summary()
    
    def print_summary(self):
        """打印测试总结"""
        self.log("\n" + "="*60, Colors.BLUE)
        self.log("测试总结", Colors.BLUE)
        self.log("="*60, Colors.BLUE)
        
        total = len(self.test_results)
        passed = sum(1 for r in self.test_results if r.success)
        failed = total - passed
        
        self.log(f"\n总测试数: {total}")
        self.log(f"通过: {passed}", Colors.GREEN if passed > 0 else Colors.RESET)
        self.log(f"失败: {failed}", Colors.RED if failed > 0 else Colors.RESET)
        
        if failed > 0:
            self.log("\n失败的测试:", Colors.RED)
            for result in self.test_results:
                if not result.success:
                    self.log(f"  - {result.name}: {result.message}", Colors.RED)
                    if result.error:
                        self.log(f"    错误: {result.error}", Colors.RED)
        
        self.log(f"\n创建的资源:", Colors.YELLOW)
        self.log(f"  - 用户: {len(self.created_users)} 个")
        self.log(f"  - 好友关系: {len(self.created_friendships)} 个")
        self.log(f"  - 群组: {len(self.created_groups)} 个")
        
        self.log("\n" + "="*60 + "\n", Colors.BLUE)

def main():
    """主函数"""
    import argparse
    
    parser = argparse.ArgumentParser(description="管理 API 测试脚本")
    parser.add_argument("--url", default=BASE_URL, help=f"服务器地址 (默认: {BASE_URL})")
    parser.add_argument("--service-key", default=SERVICE_KEY, help="Service Key")
    parser.add_argument("--config", help="配置文件路径 (JSON 格式)")
    
    args = parser.parse_args()
    
    # 从配置文件加载（如果提供）
    if args.config:
        try:
            with open(args.config, 'r') as f:
                config = json.load(f)
                base_url = config.get("base_url", args.url)
                service_key = config.get("service_key", args.service_key)
        except Exception as e:
            print(f"❌ 读取配置文件失败: {e}")
            sys.exit(1)
    else:
        base_url = args.url
        service_key = args.service_key
    
    if service_key == "your-service-key-here":
        print("⚠️  警告: 请设置正确的 SERVICE_KEY")
        print("   方法1: 修改脚本中的 SERVICE_KEY 变量")
        print("   方法2: 使用 --service-key 参数")
        print("   方法3: 使用 --config 参数指定配置文件")
        print("   注意: 服务器从环境变量 SERVICE_MASTER_KEY 读取配置")
        response = input("\n是否继续使用默认值? (y/N): ")
        if response.lower() != 'y':
            sys.exit(1)
    
    tester = AdminAPITester(base_url, service_key)
    tester.run_all_tests()

if __name__ == "__main__":
    main()
