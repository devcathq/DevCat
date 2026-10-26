// mobile/web-iroh (WebAssembly, built by mobile/web-iroh/build.sh); see its src/lib.rs.
declare module "devcat-web-iroh" {
  export default function init(): Promise<unknown>;
  export class Node {
    static create(secretHex: string, relay: string): Promise<Node>;
    id(): string;
    open(hostId: string, relay: string, home?: string | null): Promise<Pipe>;
    close(): Promise<void>;
  }
  export class Pipe {
    send(json: string): Promise<void>;
    recv(): Promise<string | null>;
    close(): void;
  }
}
