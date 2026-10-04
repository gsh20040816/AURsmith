#!/usr/bin/env python3
"""Restricted CONNECT tunnel; TLS stays between Publisher and archlinux.org."""
import ipaddress
import select
import socket
import socketserver

class Tunnel(socketserver.StreamRequestHandler):
    def handle(self):
        if ipaddress.ip_address(self.client_address[0]) not in ipaddress.ip_network('192.168.64.0/20'):
            return
        self.connection.settimeout(20)
        try:
            request = self.rfile.readline(4097)
            if len(request) > 4096 or request.split() != [b'CONNECT', b'archlinux.org:443', b'HTTP/1.1']:
                self.wfile.write(b'HTTP/1.1 403 Forbidden\r\nConnection: close\r\n\r\n')
                return
            total = 0
            while True:
                line = self.rfile.readline(4097)
                total += len(line)
                if total > 16384 or not line:
                    return
                if line == b'\r\n':
                    break
            upstream = None
            for family, kind, proto, _, address in socket.getaddrinfo('archlinux.org', 443, socket.AF_INET6, socket.SOCK_STREAM):
                candidate = socket.socket(family, kind, proto)
                candidate.settimeout(10)
                try:
                    candidate.connect(address)
                    upstream = candidate
                    break
                except OSError:
                    candidate.close()
            if upstream is None:
                self.wfile.write(b'HTTP/1.1 502 Bad Gateway\r\nConnection: close\r\n\r\n')
                return
            with upstream:
                self.wfile.write(b'HTTP/1.1 200 Connection Established\r\n\r\n')
                self.wfile.flush()
                while True:
                    ready, _, _ = select.select([self.connection, upstream], [], [], 30)
                    if not ready:
                        return
                    for source in ready:
                        data = source.recv(65536)
                        if not data:
                            return
                        destination = upstream if source is self.connection else self.connection
                        destination.sendall(data)
        except (OSError, ValueError):
            return

class Server(socketserver.ThreadingTCPServer):
    allow_reuse_address = True
    daemon_threads = True

with Server(('192.168.64.1', 19443), Tunnel) as server:
    server.serve_forever()
