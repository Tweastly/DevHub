# DevHub's installer as an image, for a server that already has Docker. It runs install.sh
# against the server's Docker (its socket) and writes DevHub in the mounted folder, which
# must keep the same path inside and outside: compose resolves its bind mounts from there.
#
#   docker run --rm -it -v /var/run/docker.sock:/var/run/docker.sock -v /opt/devhub:/opt/devhub ghcr.io/tweastly/devhub:install
FROM docker:27-cli
COPY install.sh /install.sh
ENV DEVHUB_IN_DOCKER=1
ENTRYPOINT ["sh", "/install.sh"]
