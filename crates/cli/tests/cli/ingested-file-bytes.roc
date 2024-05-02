app "ingested-file-bytes"
    packages { pf: "https://github.com/roc-lang/basic-cli/releases/download/0.10.0/vNe6s9hWzoTZtFmNkvEICPErI9ptji_ySjicO6CkucY.tar.br" }
    imports [
        pf.Stdout,
        pf.Task,
        "ingested-file.roc" as fileBytes : _, # A type hole can also be used here.
    ]
    provides [main] to pf

main =
    # Due to how fileBytes is used, it will be a List U8.
    fileBytes
        |> List.map Num.toU64
        |> List.sum
        |> Num.toStr
        |> Stdout.line!
