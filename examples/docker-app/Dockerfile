# A tiny static web server (BusyBox httpd, ~2 MB) for Aegis's Docker runtime.
# Aegis passes the port to listen on as PORT.
FROM busybox:1.36.1
COPY www/ /www/
ENV PORT=8080
EXPOSE 8080
CMD ["sh", "-c", "echo \"serving /www on port $PORT\"; exec httpd -f -v -p \"$PORT\" -h /www"]
