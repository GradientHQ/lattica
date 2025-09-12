#!/usr/bin/env python3
import sys
import time
import random
from lattica import Lattica

def main():
    args = sys.argv[1:]
    bootstrap_nodes = args if args else []
    print(f"Bootstrap nodes: {bootstrap_nodes}")

    if bootstrap_nodes:
        lattica_inst = Lattica.builder() \
            .with_bootstraps(bootstrap_nodes) \
            .build()

        try:
            print("\n=== Basic DHT ===")
            lattica_inst.store("name", "ween") # default expiration
            result = lattica_inst.get("name")
            if result:
                print(f"Retrieved value: {result.value}, expiration: {result.expiration_time}")
            else:
                print("No record found")

            print("\n=== Subkey DHT ===")
            key = "peer_list"
            peers = ["alice", "bob", "carol", "david"]

            expiration_time = time.time() + 600
            for peer in peers:
                vote = random.choice(["yes", "no", "maybe"])
                lattica_inst.store(key, vote, expiration_time, subkey=peer)

            # wait for validity
            time.sleep(2)
            votes_result = lattica_inst.get(key)

            if votes_result:
                print("All votes:")
                for peer, vote_info in votes_result.value.items():
                    print(f"  {peer}: {vote_info.value} expires: {vote_info.expiration_time}")
            else:
                print("No votes found")

        except Exception as e:
            print(f"Error during DHT operations: {e}")
            import traceback
            traceback.print_exc()
    else:
        print("\n=== Running as DHT Server ===")
        print("Waiting for connections... (Press Ctrl+C to stop)")
        Lattica.builder().build()
        try:
            while True:
                time.sleep(1)
        except KeyboardInterrupt:
            print("Server shutting down...")

# node1: python dht.py
# node2: python dht.py /ip4/127.0.0.1/tcp/x/p2p/x
if __name__ == "__main__":
    main()