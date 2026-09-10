package com.example;

import java.io.IOException;
import java.io.InputStream;
import java.nio.charset.StandardCharsets;

public final class Hello {
    private Hello() {}

    public static String greeting() throws IOException {
        try (InputStream in = Hello.class.getResourceAsStream("/greeting.txt")) {
            if (in == null) {
                throw new IOException("greeting.txt was not packaged");
            }
            return new String(in.readAllBytes(), StandardCharsets.UTF_8).trim();
        }
    }

    public static void main(String[] args) throws IOException {
        System.out.println(greeting() + (args.length > 0 ? " " + args[0] : ""));
    }
}
