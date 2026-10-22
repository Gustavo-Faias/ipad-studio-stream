/// Código partilhado entre a app anfitriã e a Broadcast Extension.
///
/// - `Protocol/`: codec PSWP v1 (`docs/PROTOCOL.md`), testado contra `protocol/test-vectors/v1.json`.
/// - `Sender/`: sessão do lado iPad: handshake, fila com prioridades e política de descarte (§7),
///   keyframes a pedido, estatísticas.
/// - `Transport/`: servidor TCP com sockets BSD/Winsock (USB em 127.0.0.1:23456).
/// - `Diagnostics/`: medições dos spikes S1 (ritmo de captura), S2 (custo do encoder) e
///   S6 (memória), enviadas ao PC dentro de `SENDER_STATS.diag`.
/// - `Apple/`: encoder VideoToolbox, métricas do sistema e o pipeline da extensão.
public enum PadStudioKit {
    /// Versão desta implementação (aparece em `HELLO_ACK.server_version`).
    public static let version = "0.1.0"

    /// Versão do protocolo PSWP implementada.
    public static let protocolVersion = 1

    /// Porta TCP escutada no iPad para o túnel USB (usbmuxd).
    public static let usbPort: UInt16 = 23456

    /// Porta TCP escutada no iPad para a rede local.
    public static let lanPort: UInt16 = 23457
}
