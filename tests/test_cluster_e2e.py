import subprocess
import socket
import time
import os
import shutil

def send_cmd(port, cmd):
    s = socket.socket()
    s.connect(('127.0.0.1', port))
    s.sendall(cmd.encode('utf-8'))
    data = s.recv(4096).decode('utf-8', errors='replace')
    s.close()
    return data

def main():
    print("[+] Starting 2-node BlinkDB cluster test...")
    shutil.rmtree("/tmp/rf_cluster_1", ignore_errors=True)
    shutil.rmtree("/tmp/rf_cluster_2", ignore_errors=True)

    p1 = subprocess.Popen([
        "bin/blinkdb-server",
        "--resp-port", "6381",
        "--binary-port", "7381",
        "--threads", "2",
        "--data-dir", "/tmp/rf_cluster_1",
        "--cluster",
        "--node-id", "1"
    ])

    p2 = subprocess.Popen([
        "bin/blinkdb-server",
        "--resp-port", "6382",
        "--binary-port", "7382",
        "--threads", "2",
        "--data-dir", "/tmp/rf_cluster_2",
        "--cluster",
        "--node-id", "2"
    ])

    try:
        time.sleep(1.0)
        print("[+] Cluster nodes started successfully.")

        # Test CLUSTER NODES on Node 1
        res1 = send_cmd(6381, "*2\r\n$7\r\nCLUSTER\r\n$5\r\nNODES\r\n")
        assert "myself,master" in res1
        print("[PASS] Node 1 CLUSTER NODES verified")

        # Test CLUSTER SLOTS on Node 1
        res2 = send_cmd(6381, "*2\r\n$7\r\nCLUSTER\r\n$5\r\nSLOTS\r\n")
        assert "127.0.0.1" in res2
        print("[PASS] Node 1 CLUSTER SLOTS verified")

        # Test CLUSTER KEYSLOT
        res3 = send_cmd(6381, "*3\r\n$7\r\nCLUSTER\r\n$7\r\nKEYSLOT\r\n$3\r\nbar\r\n")
        assert ":5061" in res3
        print("[PASS] Node 1 CLUSTER KEYSLOT bar -> 5061")

        # Test Local Read/Write on Node 1
        send_cmd(6381, "*3\r\n$3\r\nSET\r\n$5\r\nlocal\r\n$3\r\nval\r\n")
        get_res = send_cmd(6381, "*2\r\n$3\r\nGET\r\n$5\r\nlocal\r\n")
        assert "val" in get_res
        print("[PASS] Local SET/GET on Node 1 verified")

        print("\n>>> ALL CLUSTER END-TO-END TESTS PASSED! <<<")
    finally:
        p1.terminate()
        p2.terminate()
        p1.wait()
        p2.wait()

if __name__ == '__main__':
    main()
