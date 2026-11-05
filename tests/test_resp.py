import socket

def send_cmd(s, cmd_str):
    s.sendall(cmd_str.encode('utf-8'))
    return s.recv(4096).decode('utf-8', errors='replace')

def main():
    s = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    s.connect(('127.0.0.1', 6379))
    print("[+] Connected to RustForge on Port 6379 (RESP3)")

    # 1. PING
    res = send_cmd(s, "*1\r\n$4\r\nPING\r\n")
    assert "+PONG" in res, f"Expected +PONG, got {res}"
    print("[PASS] PING ->", res.strip())

    # 2. SET
    res = send_cmd(s, "*3\r\n$3\r\nSET\r\n$4\r\nname\r\n$9\r\nRustForge\r\n")
    assert "+OK" in res, f"Expected +OK, got {res}"
    print("[PASS] SET name RustForge ->", res.strip())

    # 3. GET
    res = send_cmd(s, "*2\r\n$3\r\nGET\r\n$4\r\nname\r\n")
    assert "RustForge" in res, f"Expected RustForge, got {res}"
    print("[PASS] GET name ->", res.strip())

    # 4. INCR
    send_cmd(s, "*3\r\n$3\r\nSET\r\n$7\r\ncounter\r\n$2\r\n10\r\n")
    res = send_cmd(s, "*2\r\n$4\r\nINCR\r\n$7\r\ncounter\r\n")
    assert ":11" in res, f"Expected :11, got {res}"
    print("[PASS] INCR counter ->", res.strip())

    # 5. INCRBY
    res = send_cmd(s, "*3\r\n$6\r\nINCRBY\r\n$7\r\ncounter\r\n$2\r\n20\r\n")
    assert ":31" in res, f"Expected :31, got {res}"
    print("[PASS] INCRBY counter 20 ->", res.strip())

    # 6. MSET & MGET
    send_cmd(s, "*5\r\n$4\r\nMSET\r\n$2\r\nk1\r\n$2\r\nv1\r\n$2\r\nk2\r\n$2\r\nv2\r\n")
    res = send_cmd(s, "*3\r\n$4\r\nMGET\r\n$2\r\nk1\r\n$2\r\nk2\r\n")
    assert "v1" in res and "v2" in res, f"Expected v1 and v2, got {res}"
    print("[PASS] MSET & MGET ->", res.strip().replace("\r\n", " "))

    # 7. EXISTS
    res = send_cmd(s, "*2\r\n$6\r\nEXISTS\r\n$2\r\nk1\r\n")
    assert ":1" in res, f"Expected :1, got {res}"
    print("[PASS] EXISTS k1 ->", res.strip())

    # 8. DEL
    res = send_cmd(s, "*2\r\n$3\r\nDEL\r\n$2\r\nk1\r\n")
    assert ":1" in res, f"Expected :1, got {res}"
    print("[PASS] DEL k1 ->", res.strip())

    # 9. DBSIZE
    res = send_cmd(s, "*1\r\n$6\r\nDBSIZE\r\n")
    assert res.startswith(":"), f"Expected integer count, got {res}"
    print("[PASS] DBSIZE ->", res.strip())

    # 10. INFO
    res = send_cmd(s, "*1\r\n$4\r\nINFO\r\n")
    assert "rustforge_version" in res and "io_backend:io_uring" in res
    print("[PASS] INFO -> Verified server, tiering, and io_uring metadata")

    # 11. TIER STATUS
    res = send_cmd(s, "*2\r\n$4\r\nTIER\r\n$6\r\nSTATUS\r\n")
    assert "hot_keys" in res and "disk_bytes" in res
    print("[PASS] TIER STATUS ->", res.strip().split("\r\n")[1] if "\r\n" in res else res)

    # 12. CLUSTER COMMANDS
    res = send_cmd(s, "*2\r\n$7\r\nCLUSTER\r\n$5\r\nNODES\r\n")
    assert "myself,master" in res
    print("[PASS] CLUSTER NODES -> Verified cluster node topology")

    res = send_cmd(s, "*3\r\n$7\r\nCLUSTER\r\n$7\r\nKEYSLOT\r\n$3\r\nfoo\r\n")
    assert ":12182" in res
    print("[PASS] CLUSTER KEYSLOT foo ->", res.strip())

    s.close()
    print("\n>>> ALL RESP3 PROTOCOL TESTS PASSED! <<<")

if __name__ == '__main__':
    main()
