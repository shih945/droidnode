"""Sanity-check the Bookinfo manifest's port model for DroidNode.

On this platform every container shares the device's network namespace, so:
  * every containerPort must be unique per node;
  * a Service port must not equal any containerPort on the same node (D3 binds
    <loopback>:<servicePort>, which collides with 0.0.0.0:<containerPort>);
  * every targetPort must exist as a containerPort.

Usage: python deploy/bookinfo/check-ports.py
"""

import os
import yaml

PATH = os.path.join(os.path.dirname(os.path.abspath(__file__)), "bookinfo.yaml")

svc_ports = {}
container_ports = {}
docs = [d for d in yaml.safe_load_all(open(PATH, encoding="utf-8")) if d]

for d in docs:
    name = d["metadata"]["name"]
    if d["kind"] == "Service":
        port = d["spec"]["ports"][0]
        if port["port"] in svc_ports:
            print(f"CONFLICT service port {port['port']}: {svc_ports[port['port']]} vs {name}")
        svc_ports[port["port"]] = (name, port["targetPort"])
    elif d["kind"] == "Deployment":
        c = d["spec"]["template"]["spec"]["containers"][0]
        cp = c["ports"][0]["containerPort"]
        if cp in container_ports:
            print(f"CONFLICT container port {cp}: {container_ports[cp]} vs {name}")
        container_ports[cp] = name

print(f"docs={len(docs)} services={len(svc_ports)} deployments={len(container_ports)}")
print("service ports   :", sorted(svc_ports))
print("container ports :", sorted(container_ports))

overlap = set(svc_ports) & set(container_ports)
print("service/container overlap (must be empty):", sorted(overlap))

for port, (name, target) in sorted(svc_ports.items()):
    if target not in container_ports:
        print(f"WARN {name}: targetPort {target} has no matching containerPort")
print("targetPort check done")
