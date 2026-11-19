# Bookinfo（当前清单）

Istio Bookinfo 示例的 4 个微服务，全部跑在 `bookinfo` 命名空间，每个服务 1 个副本，
镜像版本统一为 `1.20.3`。

**入口**：`http://<控制面IP>:30080/productpage`（NodePort，见下文「访问」）。

```
浏览器
  └─► productpage (Python/Flask, :8080)
        ├─► details (Ruby, :9081)
        └─► reviews (Java/OpenLiberty, :9080)
                └─► ratings (Node.js, :9082)
```

## Pod

| Pod（Deployment） | 镜像 | 语言 / 运行时 | 容器端口 | 启动方式 | readiness |
|---|---|---|---|---|---|
| `productpage-v1` | `istio/examples-bookinfo-productpage-v1:1.20.3` | Python / Flask | **8080** | `python /opt/microservices/productpage.py 8080` | `GET /health:8080` |
| `details-v1` | `istio/examples-bookinfo-details-v1:1.20.3` | Ruby | **9081** | `ruby /opt/microservices/details.rb 9081` | `GET /health:9081` |
| `reviews-v3` | `istio/examples-bookinfo-reviews-v3:1.20.3` | Java / OpenLiberty | **9080** | 镜像默认（`server.xml` 里写死 9080） | `GET /health:9080` |
| `ratings-v1` | `istio/examples-bookinfo-ratings-v1:1.20.3` | Node.js | **9082** | `node /opt/microservices/ratings.js 9082` | `GET /health:9082` |

资源只声明了 `requests`（没有 `limits`）：

| Pod | requests |
|---|---|
| productpage-v1 | cpu 100m / memory 128Mi |
| details-v1 | cpu 50m / memory 64Mi |
| reviews-v3 | cpu 100m / memory 256Mi |
| ratings-v1 | cpu 50m / memory 64Mi |

## Service

| Service | 类型 | `port`（集群内） | `targetPort`（容器） | `nodePort`（集群外） | 选择器 |
|---|---|---|---|---|---|
| `productpage` | **NodePort** | 19080 | 8080 | **30080** | `app=productpage` |
| `details` | ClusterIP | 19081 | 9081 | — | `app=details` |
| `reviews` | ClusterIP | 19083 | 9080 | — | `app=reviews` |
| `ratings` | ClusterIP | 19082 | 9082 | — | `app=ratings` |

**为什么 Service 端口是 1908x 而容器端口是 908x**：同一台设备上所有容器共享网络栈，
端口不能重复，所以容器各占一个 908x。Service 端口再用一段错开的 1908x，避免和容器
自己监听的端口撞车（DroidNode 的 Service 名代理会占用 Service 端口做本地监听）。

## 访问

**固定入口（推荐）**：`productpage` 是 `type: NodePort`，`nodePort: 30080`，
集群里每个节点（包括控制面）都监听这个端口，手机 IP 变了也不用改地址：

```bash
curl -s http://192.168.2.7:30080/productpage | grep -o 'Reviews is healthy'
# 浏览器打开 http://192.168.2.7:30080/productpage
```

`externalTrafficPolicy` 保持 `Cluster`（默认）：控制面本身不跑 productpage，
设成 `Local` 会让控制面直接拒绝这个端口。

**直连节点（排障用）**：容器直接绑在设备网络栈上，也可以直接访问容器端口：

```bash
# <节点IP> 换成 productpage 所在节点的局域网地址
curl -s http://<节点IP>:8080/productpage
```

**集群内**：用 Service 名 + Service 端口，例如 `http://details:19081`。

## 服务之间怎么找到对方

application 里不写地址、只写服务名和 Service 端口，运行时由平台解析：

| 调用方 | 环境变量 | 指向 |
|---|---|---|
| productpage | `DETAILS_HOSTNAME=details` / `DETAILS_SERVICE_PORT=19081` | details |
| productpage | `REVIEWS_HOSTNAME=reviews` / `REVIEWS_SERVICE_PORT=19083` | reviews |
| productpage | `RATINGS_HOSTNAME=ratings` / `RATINGS_SERVICE_PORT=19082` | ratings（v3 不直接用，但保留） |
| reviews | `RATINGS_HOSTNAME=ratings` / `RATINGS_SERVICE_PORT=19082` | ratings |
| reviews | `LOG_DIR=/tmp/logs` | 日志目录 |

`reviews` 的三个版本（v1 无星 / v2 黑星 / v3 红星）由镜像决定，当前清单只跑 **v3**。

## 部署与查看

```bash
kubectl apply -f deploy/bookinfo/bookinfo.yaml

kubectl get pods   -n bookinfo -o wide
kubectl get svc    -n bookinfo
kubectl get deploy -n bookinfo

kubectl delete -f deploy/bookinfo/bookinfo.yaml
```

## 文件

| 文件 | 内容 |
|---|---|
| `bookinfo.yaml` | 全部对象：1 个 Namespace + 4 个 Service + 4 个 Deployment（共 9 个） |
| `check-ports.py` | 校验 Service 端口与容器端口不重叠、`targetPort` 在 Deployment 里存在 |

```bash
python deploy/bookinfo/check-ports.py
```
