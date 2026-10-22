import ReplayKit
import SwiftUI

/// Ecrã principal: instruções e o botão do sistema que inicia a transmissão.
/// A configuração do encoder e o diagnóstico ficam no PC (separador iPad da app PadStudio).
struct ContentView: View {
    var body: some View {
        VStack(spacing: 28) {
            Text("PadStudio")
                .font(.largeTitle.bold())
            VStack(alignment: .leading, spacing: 10) {
                Label("Liga o iPad ao PC por cabo e abre a app PadStudio no PC.", systemImage: "cable.connector")
                Label("Toca no botão abaixo e escolhe «Iniciar transmissão».", systemImage: "record.circle")
                Label("Abre o jogo. A imagem, a latência e o diagnóstico aparecem no PC.", systemImage: "gamecontroller")
            }
            .foregroundStyle(.secondary)
            BroadcastPickerButton()
                .frame(width: 88, height: 88)
            Text("Para parar, toca na barra vermelha no topo do ecrã ou no botão outra vez.")
                .font(.footnote)
                .foregroundStyle(.tertiary)
                .multilineTextAlignment(.center)
        }
        .padding(40)
    }
}

/// Botão do sistema que abre a folha de transmissão do ReplayKit já apontada à nossa extensão.
struct BroadcastPickerButton: UIViewRepresentable {
    func makeUIView(context _: Context) -> RPSystemBroadcastPickerView {
        let picker = RPSystemBroadcastPickerView(frame: .zero)
        picker.preferredExtension = Bundle.main.bundleIdentifier.map { $0 + ".broadcast" }
        // O microfone do iPad entra no passo do áudio; por agora a transmissão é só vídeo.
        picker.showsMicrophoneButton = false
        return picker
    }

    func updateUIView(_: RPSystemBroadcastPickerView, context _: Context) {}
}
