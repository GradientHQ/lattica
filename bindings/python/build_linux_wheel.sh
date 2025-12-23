#!/bin/bash
set -e

# 在 macOS 上使用 Docker 交叉编译 Linux x86_64 wheel 包

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"

echo "==> 构建 Linux x86_64 wheel 包"
echo "项目目录: $PROJECT_ROOT"

# 检查 Docker
if ! command -v docker &> /dev/null; then
    echo "错误: 未安装 Docker"
    echo "请从 https://www.docker.com/products/docker-desktop 安装 Docker Desktop"
    exit 1
fi

if ! docker info &> /dev/null; then
    echo "错误: Docker 未运行，请启动 Docker Desktop"
    exit 1
fi

# 使用官方 maturin 镜像（latest 版本支持 nightly Rust）
DOCKER_IMAGE="ghcr.io/pyo3/maturin:latest"

echo "==> 拉取 Docker 镜像..."
docker pull $DOCKER_IMAGE

# 创建输出目录
mkdir -p "$SCRIPT_DIR/target/wheels/linux"

# 在 Docker 容器中构建
echo "==> 开始构建..."
docker run --rm \
    --platform linux/amd64 \
    -v "$PROJECT_ROOT:/io" \
    -w /io/bindings/python \
    $DOCKER_IMAGE \
    build --release --target x86_64-unknown-linux-gnu --out target/wheels/linux

if [ $? -eq 0 ]; then
    echo ""
    echo "==> 构建成功！"
    echo "输出位置: $SCRIPT_DIR/target/wheels/linux/"
    echo ""
    ls -lh "$SCRIPT_DIR/target/wheels/linux/"
    echo ""
    echo "安装命令: pip install $SCRIPT_DIR/target/wheels/linux/*.whl"
else
    echo "==> 构建失败"
    exit 1
fi
