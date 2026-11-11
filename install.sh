#!/bin/bash
# 安装 powermode-daemon 为系统服务。
#
# 用法：
#   ./install.sh            安装
#   ./install.sh uninstall  卸载
set -eu

ROOT="$(cd "$(dirname "$0")" && pwd)"
BIN=/usr/local/bin/powermode-daemon
UNIT=/etc/systemd/system/powermode-daemon.service
CONF_DIR=/etc/powermode

SUDO=""
[ "$(id -u)" -ne 0 ] && SUDO="sudo"

case "${1:-install}" in
uninstall)
    $SUDO systemctl disable --now powermode-daemon.service 2>/dev/null || true
    $SUDO rm -f "$UNIT" "$BIN"
    $SUDO systemctl daemon-reload
    echo "已卸载（配置保留在 $CONF_DIR）"
    exit 0
    ;;
esac

echo "→ 构建"
cd "$ROOT"
cargo build --release

echo "→ 安装二进制"
$SUDO install -m 755 target/release/powermode-daemon "$BIN"

echo "→ 安装 systemd 服务"
$SUDO install -m 644 systemd/powermode-daemon.service "$UNIT"
$SUDO systemctl daemon-reload

echo "→ 创建配置目录"
$SUDO mkdir -p "$CONF_DIR"
if [ ! -f "$CONF_DIR/fast-list.conf" ]; then
    $SUDO tee "$CONF_DIR/fast-list.conf" >/dev/null <<'EOF'
# fast 名单 —— 名单里的进程一运行就自动进性能档
#
# 格式：<进程名子串> <档位>
# 档位：powersave | balance | performance | fast
#
# 顺序敏感：第一个匹配的生效，具体规则要放前面。
# 例如 "steam_app_730" 包含 "steam"，steam 规则必须排在后面。

# Steam 与兼容层（最重负载）
steam_app_          performance
box64               performance
box86               performance
FEXBash             performance
FEXInterpreter      performance
gamescope           performance
proton              performance
wineserver          performance
wine                performance

# 商业手游
com.tencent.tmgp    performance
com.activision      performance
com.epicgames       performance
com.riotgames       performance
com.gameloft        performance
com.supercell       performance
cn.hoyoverse        performance
com.miHoYo          performance
com.pubg            performance
com.niantic         performance
com.rovio           performance
EOF
    echo "  已创建默认名单"
fi

echo
echo "安装完成。"
echo "  二进制: $BIN"
echo "  服务:   $UNIT"
echo "  名单:   $CONF_DIR/fast-list.conf"
echo
echo "启用方式：在扩展设置页点「启动」，或手动："
echo "  echo 1 > /run/powermode/extension-enabled"
echo "  echo 1 > /run/powermode/daemon-enabled"
echo "  $SUDO systemctl enable --now powermode-daemon"
