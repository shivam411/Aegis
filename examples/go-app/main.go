package main

import (
	"fmt"
	"net/http"
)

func handler(w http.ResponseWriter, r *http.Request) {
	fmt.Fprintf(w, `{"status":"ok","runtime":"Go"}`)
}

func main() {
	http.HandleFunc("/health", handler)
	fmt.Println("Aegis Sample Go Service running on port 8080...")
	http.ListenAndServe(":8080", nil)
}
