package com.example.bookmarks;

import org.springframework.boot.SpringApplication;
import org.springframework.boot.autoconfigure.SpringBootApplication;
import org.springframework.boot.context.properties.ConfigurationPropertiesScan;

/** The application: one class, one annotation, one `main`. */
@SpringBootApplication
@ConfigurationPropertiesScan
public class BookmarksApplication {

    public static void main(String[] args) {
        SpringApplication.run(BookmarksApplication.class, args);
    }
}
