import socket
import struct

RF_MAGIC = b'RF'
OP_GET = 0x01
OP_SET = 0x02
OP_DEL = 0x03
OP_EXISTS = 0x04
OP_INCR = 0x05
OP_PING = 0x06
OP_STATS = 0x07

STATUS_OK = 0x00
STATUS_NOT_FOUND = 0x01
STATUS_ERR = 0x02

def encode_request(opcode, req_id, key=b"", value=b"", ttl=0, flags=0):
    header = struct.pack(
        "<2sBBHIIHI",
        RF_MAGIC,
        1,       # version
        opcode,  # opcode
        flags,   # flags
        req_id,  # req_id
        ttl,     # ttl
        len(key),
        len(value)
    )
    return header + key + value

def decode_response(data):
    assert len(data) >= 13, f"Response too short: {len(data)}"
    magic, status, req_id, flags, val_len = struct.unpack("<2sBIHI", data[:13])
    assert magic == RF_MAGIC, f"Invalid magic: {magic}"
    val = data[13:13 + val_len]
    return status, req_id, flags, val

def main():
    s = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    s.connect(('127.0.0.1', 7379))
    print("[+] Connected to RustForge on Port 7379 (Native Binary Protocol)")

    # 1. PING
    s.sendall(encode_request(OP_PING, 100))
    status, req_id, _, val = decode_response(s.recv(1024))
    assert status == STATUS_OK and req_id == 100 and val == b"PONG"
    print("[PASS] Binary OP_PING -> PONG (Req ID: 100)")

    # 2. SET
    s.sendall(encode_request(OP_SET, 101, key=b"bin_key", value=b"bin_val_fast"))
    status, req_id, _, _ = decode_response(s.recv(1024))
    assert status == STATUS_OK and req_id == 101
    print("[PASS] Binary OP_SET -> OK (Req ID: 101)")

    # 3. GET
    s.sendall(encode_request(OP_GET, 102, key=b"bin_key"))
    status, req_id, _, val = decode_response(s.recv(1024))
    assert status == STATUS_OK and req_id == 102 and val == b"bin_val_fast"
    print("[PASS] Binary OP_GET -> bin_val_fast (Req ID: 102)")

    # 4. INCR
    s.sendall(encode_request(OP_SET, 103, key=b"bin_cnt", value=b"50"))
    decode_response(s.recv(1024))
    s.sendall(encode_request(OP_INCR, 104, key=b"bin_cnt", value=b"5"))
    status, req_id, _, val = decode_response(s.recv(1024))
    assert status == STATUS_OK and val == b"55"
    print("[PASS] Binary OP_INCR -> 55 (Req ID: 104)")

    # 5. EXISTS
    s.sendall(encode_request(OP_EXISTS, 105, key=b"bin_key"))
    status, _, _, val = decode_response(s.recv(1024))
    assert status == STATUS_OK and val == b"\x01"
    print("[PASS] Binary OP_EXISTS -> 1 (Req ID: 105)")

    # 6. DEL
    s.sendall(encode_request(OP_DEL, 106, key=b"bin_key"))
    status, _, _, _ = decode_response(s.recv(1024))
    assert status == STATUS_OK
    print("[PASS] Binary OP_DEL -> OK (Req ID: 106)")

    # 7. GET after DEL -> STATUS_NOT_FOUND
    s.sendall(encode_request(OP_GET, 107, key=b"bin_key"))
    status, req_id, _, _ = decode_response(s.recv(1024))
    assert status == STATUS_NOT_FOUND
    print("[PASS] Binary OP_GET after DEL -> STATUS_NOT_FOUND")

    # 8. STATS
    s.sendall(encode_request(OP_STATS, 108))
    status, _, _, val = decode_response(s.recv(1024))
    assert status == STATUS_OK and b"dbsize:" in val
    print("[PASS] Binary OP_STATS ->", val.decode('utf-8'))

    s.close()
    print("\n>>> ALL NATIVE BINARY PROTOCOL TESTS PASSED! <<<")

if __name__ == '__main__':
    main()
