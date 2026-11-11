# DevHub's installer as an image, for a server that already has Docker. It runs install.sh
# against the server's Docker (its socket only): DevHub's container and volumes, nothing
# written on the server outside Docker.
#
#   docker run --rm -it -v /var/run/docker.sock:/var/run/docker.sock ghcr.io/tweastly/devhub:install
FROM docker:27-cli
COPY install.sh /install.sh
ENV DEVHUB_IN_DOCKER=1
ENTRYPOINT ["sh", "/install.sh"]
