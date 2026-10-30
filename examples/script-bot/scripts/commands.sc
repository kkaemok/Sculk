options:
    prefixes: "/", "!"

command ping:
    trigger:
        reply "Pong!"

command greet <name>:
    aliases: hello, 인사
    prefixes: "!", ">>", "봇아 "
    trigger:
        set {_name} to arg-1
        send "Hello, %{_name}%!" to event-room
