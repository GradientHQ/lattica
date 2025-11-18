#!/usr/bin/env python3
import sys
import time
from lattica import Lattica

def main():
    try:
        args = sys.argv[1:]
        bootstrap_nodes = [args[0]] if args else []
        request_cid = args[1] if args else None

        # init lattica
        lattica = Lattica.builder() \
            .with_bootstraps(bootstrap_nodes) \
            .build()

        # wait for connection
        time.sleep(1)

        if bootstrap_nodes:
            print("request_cid: {}".format(request_cid))

            peer_list = lattica.get_providers(request_cid)
            print(f"peer list: {peer_list}")

            try:
                # Use default timeout (10 seconds)
                # You can also specify a custom timeout: lattica.get_block(request_cid, timeout_secs=30)
                data = lattica.get_block(request_cid)
                print(f"data: {data}")
            except Exception as e:
                print(f"Failed to get block: {e}")
                print("This may happen if the CID doesn't exist or the request timed out.")

        else:
            # put block
            cid = lattica.put_block(b'hello')
            print(f"put block success, cid: {cid}")

            # start providing
            lattica.start_providing(cid)

        while True:
            time.sleep(1)
    except KeyboardInterrupt:
        print("KeyboardInterrupt")
        pass
    except Exception as e:
        print(e)

# node1: python bitswap.py
# node2: python bitswap.py /ip4/127.0.0.1/tcp/x/p2p/x  xxx(cid)
if __name__ == "__main__":
    main()